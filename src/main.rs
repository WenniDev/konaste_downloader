use anyhow::Result;
use clap::Parser;
use kbinxml;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A simple resource downloader
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// URL to fetch resource information from
    #[arg(short, long)]
    url: String,

    /// Path to save downloaded resources
    #[arg(short, long, default_value = ".")]
    output: String,

    /// Number of concurrent downloads
    #[arg(short, long, default_value_t = 4)]
    concurrency: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let client = Client::builder().build()?;

    let resource_info: ResourceInfo = {
        let response = client.get(&args.url).send().await?;

        let body = response.bytes().await?;
        let body = match kbinxml::from_binary(body.clone().into()) {
            Ok((nodes, _)) => kbinxml::to_text_xml(&nodes)?,
            Err(_) => body.into(),
        };

        quick_xml::de::from_reader(body.as_slice())?
    };

    #[derive(Default)]
    struct Status {
        downloaded_files: AtomicUsize,
        downloaded_bytes: AtomicUsize,
    }

    let status = Arc::new(Status::default());
    let total = resource_info.files.len();
    let total_len = resource_info
        .files
        .iter()
        .map(|f| f.size as usize)
        .sum::<usize>();

    let semaphore = Arc::new(tokio::sync::Semaphore::new(args.concurrency));
    let mut handles = Vec::new();
    for file in resource_info.files {
        let client = client.clone();
        let output_path = args.output.clone();
        let permit = semaphore.clone().acquire_owned().await?;
        let status = status.clone();

        let handle = tokio::spawn(async move {
            let file_path = std::path::Path::new(&output_path).join(&file.path);

            // Check if file exists and verify hash
            if file_path.exists() {
                if let Ok(existing_data) = tokio::fs::read(&file_path).await {
                    let mut hasher = Sha256::new();
                    hasher.update(&existing_data);
                    let hash = format!("{:x}", hasher.finalize());

                    // If hash matches, skip download
                    if hash == file.sum {
                        let done = status.downloaded_files.fetch_add(1, Ordering::SeqCst) + 1;
                        let done_bytes = status
                            .downloaded_bytes
                            .fetch_add(file.size as usize, Ordering::SeqCst)
                            + (file.size as usize);
                        let pct = (done_bytes as f64 / total_len as f64) * 100.0;
                        println!(
                            "Skipped (unchanged): {} - Progress: {}/{} files ({}%)",
                            file.path, done, total, pct
                        );
                        return Ok(());
                    }
                }
            }

            let _permit = permit; // Keep the permit alive for the duration of the download
            let response = client.get(&file.url).send().await?;
            let bytes = response.bytes().await?;

            if let Some(parent) = file_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&file_path, &bytes).await?;

            // Increment counter and print progress after successful download
            let done = status.downloaded_files.fetch_add(1, Ordering::SeqCst) + 1;
            let done_bytes = status
                .downloaded_bytes
                .fetch_add(file.size as usize, Ordering::SeqCst)
                + (file.size as usize);
            let pct = (done_bytes as f64 / total_len as f64) * 100.0;
            println!(
                "Downloaded: {} - Progress: {}/{} files ({}%)",
                file.path, done, total, pct
            );

            Ok::<(), anyhow::Error>(())
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await??;
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct ResourceInfo {
    #[serde(rename = "$value", default)]
    files: Vec<FileResource>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct FileResource {
    #[serde(default)]
    path: String,
    #[serde(default)]
    version: i32,
    #[serde(default)]
    size: i32,
    #[serde(default)]
    sum: String,
    #[serde(default)]
    url: String,
}
