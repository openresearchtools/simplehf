//! SimpleHF's authenticated, concurrent ranged-download worker.
//!
//! The architecture and adaptive chunk strategy are derived from
//! rust-hf-downloader by Johannes Bertens, used under the MIT License. See
//! THIRD_PARTY_NOTICES.md and licenses/rust-hf-downloader-MIT.txt.

use futures_util::StreamExt;
use reqwest::{header, Client, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Semaphore;

const MIN_CHUNK: u64 = 8 * 1024 * 1024;
const MAX_CHUNK: u64 = 128 * 1024 * 1024;
const TARGET_CHUNKS: u64 = 24;
const RETRIES: usize = 5;

#[derive(Deserialize)]
struct Manifest {
    repo_id: String,
    destination: PathBuf,
    files: Vec<ManifestFile>,
    #[serde(default = "default_connections")]
    connections: usize,
}

#[derive(Clone, Deserialize)]
struct ManifestFile {
    path: String,
    size: Option<u64>,
    sha256: Option<String>,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event<'a> {
    Job {
        status: &'a str,
        error: Option<String>,
    },
    File {
        index: usize,
        status: &'a str,
        downloaded: u64,
        total: u64,
        error: Option<String>,
    },
}

struct FileState {
    downloaded: AtomicU64,
    total: u64,
    last_emit: Mutex<Instant>,
}

#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Serialize, Deserialize)]
struct ByteRange {
    start: u64,
    end: u64,
}

fn default_connections() -> usize {
    8
}

fn emit(event: Event<'_>) {
    println!(
        "{}",
        serde_json::to_string(&event).expect("event serialization")
    );
}

fn validate_repo_id(repo_id: &str) -> Result<(), String> {
    let parts: Vec<_> = repo_id.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty() || *part == "." || *part == ".." || part.contains(['\\', '\0'])
        })
    {
        return Err("repository ID must be organization/model".into());
    }
    Ok(())
}

fn safe_relative(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute()
        || value.contains('\\')
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!("unsafe repository path: {value}"));
    }
    Ok(path.to_path_buf())
}

fn chunk_ranges(size: u64) -> Vec<ByteRange> {
    if size == 0 {
        return vec![];
    }
    let chunk = (size / TARGET_CHUNKS).clamp(MIN_CHUNK, MAX_CHUNK);
    (0..size)
        .step_by(chunk as usize)
        .map(|start| ByteRange {
            start,
            end: (start + chunk - 1).min(size - 1),
        })
        .collect()
}

async fn probe(client: &Client, url: &str, hinted: Option<u64>) -> Result<(u64, bool), String> {
    let response = client
        .get(url)
        .header(header::RANGE, "bytes=0-0")
        .send()
        .await
        .map_err(|error| format!("size request failed: {error}"))?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Err("access denied; check the token and gated-model access".into());
    }
    let response = response
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let ranged = response.status() == StatusCode::PARTIAL_CONTENT;
    let total = response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('/').next())
        .and_then(|value| value.parse().ok())
        .or(hinted)
        .or_else(|| response.content_length())
        .ok_or_else(|| "server did not report a file size".to_string())?;
    Ok((total, ranged))
}

fn read_completed(path: &Path) -> BTreeSet<ByteRange> {
    std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn write_completed(path: &Path, ranges: &BTreeSet<ByteRange>) -> Result<(), String> {
    let temp = path.with_extension("ranges.tmp");
    std::fs::write(&temp, serde_json::to_vec(ranges).unwrap())
        .map_err(|error| error.to_string())?;
    std::fs::rename(temp, path).map_err(|error| error.to_string())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", digest.finalize()))
}

async fn fetch_range(
    client: &Client,
    url: &str,
    partial: &Path,
    range: ByteRange,
    index: usize,
    state: &FileState,
) -> Result<u64, String> {
    let expected = range.end - range.start + 1;
    let mut last_error = String::new();
    for attempt in 0..=RETRIES {
        let mut received = 0;
        let result = async {
            let response = client
                .get(url)
                .header(
                    header::RANGE,
                    format!("bytes={}-{}", range.start, range.end),
                )
                .send()
                .await
                .map_err(|error| error.to_string())?;
            if response.status() != StatusCode::PARTIAL_CONTENT {
                return Err(format!(
                    "server ignored byte range (HTTP {})",
                    response.status()
                ));
            }
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .open(partial)
                .await
                .map_err(|error| error.to_string())?;
            file.seek(std::io::SeekFrom::Start(range.start))
                .await
                .map_err(|error| error.to_string())?;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| error.to_string())?;
                received += chunk.len() as u64;
                if received > expected {
                    return Err("range response exceeded requested size".into());
                }
                file.write_all(&chunk)
                    .await
                    .map_err(|error| error.to_string())?;
                let downloaded = state
                    .downloaded
                    .fetch_add(chunk.len() as u64, Ordering::Relaxed)
                    + chunk.len() as u64;
                maybe_emit(index, state, downloaded, "downloading");
            }
            file.flush().await.map_err(|error| error.to_string())?;
            if received != expected {
                return Err(format!(
                    "short range: expected {expected}, received {received}"
                ));
            }
            Ok(received)
        }
        .await;
        match result {
            Ok(value) => return Ok(value),
            Err(error) => {
                if received > 0 {
                    state.downloaded.fetch_sub(received, Ordering::Relaxed);
                }
                last_error = error;
            }
        }
        if attempt < RETRIES {
            tokio::time::sleep(Duration::from_secs(1 << attempt.min(4))).await;
        }
    }
    Err(last_error)
}

async fn fetch_whole(
    client: &Client,
    url: &str,
    partial: &Path,
    index: usize,
    state: &FileState,
) -> Result<(), String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    let mut output = tokio::fs::File::create(partial)
        .await
        .map_err(|error| error.to_string())?;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        output
            .write_all(&chunk)
            .await
            .map_err(|error| error.to_string())?;
        let downloaded = state
            .downloaded
            .fetch_add(chunk.len() as u64, Ordering::Relaxed)
            + chunk.len() as u64;
        maybe_emit(index, state, downloaded, "downloading");
    }
    output.flush().await.map_err(|error| error.to_string())
}

fn maybe_emit(index: usize, state: &FileState, downloaded: u64, status: &'static str) {
    let mut last = state.last_emit.lock().unwrap();
    if last.elapsed() >= Duration::from_millis(200) || downloaded == state.total {
        *last = Instant::now();
        emit(Event::File {
            index,
            status,
            downloaded,
            total: state.total,
            error: None,
        });
    }
}

async fn download_file(
    client: Client,
    semaphore: Arc<Semaphore>,
    manifest: Arc<Manifest>,
    index: usize,
) -> Result<(), String> {
    let item = &manifest.files[index];
    let relative = safe_relative(&item.path)?;
    let final_path = manifest.destination.join(&manifest.repo_id).join(relative);
    let partial = final_path.with_file_name(format!(
        "{}.part",
        final_path.file_name().unwrap().to_string_lossy()
    ));
    let ranges_path = partial.with_extension("part.ranges");
    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}?download=true",
        manifest.repo_id,
        item.path
            .split('/')
            .map(urlencoding::encode)
            .collect::<Vec<_>>()
            .join("/")
    );
    let (total, ranged) = probe(&client, &url, item.size).await?;
    if final_path
        .metadata()
        .map(|meta| meta.len() == total)
        .unwrap_or(false)
        && item
            .sha256
            .as_ref()
            .map(|expected| {
                sha256_file(&final_path)
                    .map(|actual| actual == *expected)
                    .unwrap_or(false)
            })
            .unwrap_or(true)
    {
        emit(Event::File {
            index,
            status: "complete",
            downloaded: total,
            total,
            error: None,
        });
        return Ok(());
    }
    emit(Event::File {
        index,
        status: "downloading",
        downloaded: 0,
        total,
        error: None,
    });
    let state = Arc::new(FileState {
        downloaded: AtomicU64::new(0),
        total,
        last_emit: Mutex::new(Instant::now()),
    });
    if ranged {
        let completed = Arc::new(Mutex::new(read_completed(&ranges_path)));
        let already = completed
            .lock()
            .unwrap()
            .iter()
            .map(|range| range.end - range.start + 1)
            .sum();
        state.downloaded.store(already, Ordering::Relaxed);
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&partial)
            .await
            .map_err(|error| error.to_string())?;
        file.set_len(total)
            .await
            .map_err(|error| error.to_string())?;
        drop(file);
        let pending: Vec<_> = chunk_ranges(total)
            .into_iter()
            .filter(|range| !completed.lock().unwrap().contains(range))
            .collect();
        let mut tasks = Vec::new();
        for range in pending {
            let (client, permit_pool, partial, ranges_path, completed, state, url) = (
                client.clone(),
                semaphore.clone(),
                partial.clone(),
                ranges_path.clone(),
                completed.clone(),
                state.clone(),
                url.clone(),
            );
            tasks.push(tokio::spawn(async move {
                let _permit = permit_pool
                    .acquire()
                    .await
                    .map_err(|error| error.to_string())?;
                fetch_range(&client, &url, &partial, range, index, &state).await?;
                {
                    let mut done = completed.lock().unwrap();
                    done.insert(range);
                    write_completed(&ranges_path, &done)?;
                }
                Ok::<(), String>(())
            }));
        }
        let mut range_error = None;
        for task in tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    range_error.get_or_insert(error);
                }
                Err(error) => {
                    range_error.get_or_insert(error.to_string());
                }
            };
        }
        if let Some(error) = range_error {
            return Err(error);
        }
    } else {
        let _permit = semaphore
            .acquire()
            .await
            .map_err(|error| error.to_string())?;
        let mut last_error = None;
        for attempt in 0..=RETRIES {
            state.downloaded.store(0, Ordering::Relaxed);
            match fetch_whole(&client, &url, &partial, index, &state).await {
                Ok(()) => {
                    last_error = None;
                    break;
                }
                Err(error) => last_error = Some(error),
            }
            if attempt < RETRIES {
                tokio::time::sleep(Duration::from_secs(1 << attempt.min(4))).await;
            }
        }
        if let Some(error) = last_error {
            return Err(error);
        }
    }
    let actual = tokio::fs::metadata(&partial)
        .await
        .map_err(|error| error.to_string())?
        .len();
    if actual != total {
        return Err(format!(
            "size mismatch: expected {total}, received {actual}"
        ));
    }
    if let Some(expected) = &item.sha256 {
        let actual = sha256_file(&partial)?;
        if actual != *expected {
            let _ = tokio::fs::remove_file(&ranges_path).await;
            return Err(format!("SHA-256 mismatch for {}", item.path));
        }
    }
    tokio::fs::rename(&partial, &final_path)
        .await
        .map_err(|error| error.to_string())?;
    let _ = tokio::fs::remove_file(ranges_path).await;
    emit(Event::File {
        index,
        status: "complete",
        downloaded: total,
        total,
        error: None,
    });
    Ok(())
}

#[tokio::main]
async fn main() {
    let manifest: Manifest = match serde_json::from_reader(std::io::stdin()) {
        Ok(value) => value,
        Err(error) => {
            emit(Event::Job {
                status: "failed",
                error: Some(format!("invalid manifest: {error}")),
            });
            return;
        }
    };
    if let Err(error) = validate_repo_id(&manifest.repo_id) {
        emit(Event::Job {
            status: "failed",
            error: Some(error),
        });
        return;
    }
    let mut headers = header::HeaderMap::new();
    if let Ok(token) = std::env::var("HF_TOKEN") {
        if !token.trim().is_empty() {
            match header::HeaderValue::from_str(&format!("Bearer {}", token.trim())) {
                Ok(value) => {
                    headers.insert(header::AUTHORIZATION, value);
                }
                Err(_) => {
                    emit(Event::Job {
                        status: "failed",
                        error: Some("invalid token".into()),
                    });
                    return;
                }
            }
        }
    }
    let client = match Client::builder()
        .default_headers(headers)
        .user_agent("SimpleHF/0.2")
        .timeout(Duration::from_secs(300))
        .build()
    {
        Ok(value) => value,
        Err(error) => {
            emit(Event::Job {
                status: "failed",
                error: Some(error.to_string()),
            });
            return;
        }
    };
    emit(Event::Job {
        status: "downloading",
        error: None,
    });
    let manifest = Arc::new(manifest);
    let semaphore = Arc::new(Semaphore::new(manifest.connections.clamp(1, 32)));
    let mut tasks = Vec::new();
    for index in 0..manifest.files.len() {
        let (client, semaphore, manifest) = (client.clone(), semaphore.clone(), manifest.clone());
        tasks.push(tokio::spawn(async move {
            (
                index,
                download_file(client, semaphore, manifest, index).await,
            )
        }));
    }
    let mut failed = false;
    for task in tasks {
        match task.await {
            Ok((index, Err(error))) => {
                failed = true;
                emit(Event::File {
                    index,
                    status: "failed",
                    downloaded: 0,
                    total: 0,
                    error: Some(error),
                });
            }
            Err(error) => {
                failed = true;
                emit(Event::Job {
                    status: "failed",
                    error: Some(error.to_string()),
                });
            }
            _ => {}
        }
    }
    emit(Event::Job {
        status: if failed { "failed" } else { "complete" },
        error: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_that_escape_destination() {
        for path in ["../token", "/etc/passwd", "folder/../../secret", "a\\b"] {
            assert!(safe_relative(path).is_err(), "accepted {path}");
        }
        assert_eq!(
            safe_relative("weights/model.safetensors").unwrap(),
            PathBuf::from("weights/model.safetensors")
        );
    }

    #[test]
    fn adaptive_ranges_cover_file_exactly() {
        let size = 257 * 1024 * 1024 + 17;
        let ranges = chunk_ranges(size);
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, size - 1);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].end + 1, pair[1].start);
        }
        assert_eq!(
            ranges
                .iter()
                .map(|range| range.end - range.start + 1)
                .sum::<u64>(),
            size
        );
    }

    #[test]
    fn requires_namespaced_repository_ids() {
        assert!(validate_repo_id("org/model").is_ok());
        assert!(validate_repo_id("model").is_err());
        assert!(validate_repo_id("org/../model").is_err());
    }
}
