use hyper_connect::clients::http1::{Http1Client, HttpClient};
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::{
    env,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    manifest_type: String,
    manifest_version: u32,
    overrides: String,
    files: Vec<ManifestFile>,
}

#[derive(Deserialize)]
struct ManifestFile {
    #[serde(rename = "projectID")]
    project_id: u64,
    #[serde(rename = "fileID")]
    file_id: u64,
    required: bool,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileInfo {
    file_name: String,
    download_url: Option<String>,
    file_length: u64,
    #[serde(default)]
    hashes: Vec<FileHash>,
}

#[derive(Deserialize)]
struct FileHash {
    value: String,
    algo: u8,
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn err(message: impl Into<String>) -> Box<dyn Error> {
    io::Error::other(message.into()).into()
}

fn valid_component(name: &str) -> Result<&str> {
    let path = Path::new(name);
    if path.components().count() != 1
        || path.file_name().and_then(|s| s.to_str()) != Some(name)
        || name == "."
        || name == ".."
    {
        return Err(err(format!("unsafe path component: {name:?}")));
    }
    Ok(name)
}

fn copy_overrides(source: &Path, destination: &Path) -> Result<()> {
    if !source.exists() {
        return Ok(());
    }
    if !source.is_dir() {
        return Err(err(format!(
            "overrides is not a directory: {}",
            source.display()
        )));
    }
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = destination.join(entry.file_name());
        if kind.is_symlink() {
            return Err(err(format!(
                "symlink in overrides: {}",
                entry.path().display()
            )));
        }
        if target.is_symlink() {
            return Err(err(format!("symlink in destination: {}", target.display())));
        }
        if kind.is_dir() {
            if target.is_file() {
                return Err(err(format!(
                    "override directory conflicts with file: {}",
                    target.display()
                )));
            }
            fs::create_dir_all(&target)?;
            copy_overrides(&entry.path(), &target)?;
        } else if kind.is_file() {
            if target.is_dir() {
                return Err(err(format!(
                    "override file conflicts with directory: {}",
                    target.display()
                )));
            }
            fs::copy(entry.path(), target)?;
        } else {
            return Err(err(format!(
                "unsupported override entry: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

async fn api_json<T: serde::de::DeserializeOwned>(
    client: &Http1Client,
    key: &str,
    url: &str,
) -> Result<T> {
    let response = client
        .get(url)
        .headers(vec![("x-api-key", key.to_owned())])
        .send()
        .await?;
    Ok(response.to_json()?)
}

async fn download(client: &Http1Client, key: &str, url: &str) -> Result<Vec<u8>> {
    if !url.starts_with("https://") {
        return Err(err(format!("insecure download URL: {url}")));
    }
    let authority = url[8..]
        .split('/')
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("");
    let response = if authority == "edge.forgecdn.net" {
        client
            .get(url)
            .headers(vec![("x-api-key", key.to_owned())])
            .send()
            .await?
    } else {
        client.get(url).send().await?
    };
    Ok(response.data.to_vec())
}

fn sha1_matches(bytes: &[u8], hashes: &[FileHash]) -> Result<()> {
    if let Some(expected) = hashes.iter().find(|hash| hash.algo == 1) {
        let actual = format!("{:x}", Sha1::digest(bytes));
        if !actual.eq_ignore_ascii_case(&expected.value) {
            return Err(err(format!(
                "SHA-1 mismatch: expected {}, got {actual}",
                expected.value
            )));
        }
    }
    Ok(())
}

async fn run() -> Result<()> {
    let mut args = env::args_os();
    let program = args.next().unwrap_or_default();
    let Some(folder) = args.next() else {
        return Err(err(format!("usage: {} <unpacked-modpack-folder>\nSet CURSEFORGE_API_KEY to your CurseForge API key.", Path::new(&program).display())));
    };
    if args.next().is_some() {
        return Err(err("expected exactly one folder argument"));
    }
    let folder = PathBuf::from(folder);
    let manifest: Manifest = serde_json::from_slice(&fs::read(folder.join("manifest.json"))?)?;
    if manifest.manifest_type != "minecraftModpack" || manifest.manifest_version != 1 {
        return Err(err("unsupported CurseForge manifest type or version"));
    }
    let overrides = folder.join(valid_component(&manifest.overrides)?);
    let key = env::var("CURSEFORGE_API_KEY").map_err(|_| err("CURSEFORGE_API_KEY is required"))?;
    if key.is_empty() {
        return Err(err("CURSEFORGE_API_KEY is empty"));
    }
    let destination = folder.join("downloaded");
    fs::create_dir_all(destination.join("mods"))?;
    let client = Http1Client::new(4);
    let mut failed = Vec::new();
    for (index, file) in manifest.files.iter().enumerate() {
        let url = format!(
            "https://api.curseforge.com/v1/mods/{}/files/{}",
            file.project_id, file.file_id
        );
        let result: Result<()> = async {
            let info: Envelope<FileInfo> = api_json(&client, &key, &url).await?;
            let name = valid_component(&info.data.file_name)?;
            let target = destination.join("mods").join(name);
            if target.is_symlink() { return Err(err(format!("symlink in destination: {}", target.display()))); }
            if target.exists() {
                if !target.is_file() { return Err(err(format!("destination is not a file: {}", target.display()))); }
                let existing = fs::read(&target)?;
                if existing.len() as u64 == info.data.file_length
                    && sha1_matches(&existing, &info.data.hashes).is_ok() {
                    println!("[{}/{}] already present: {name}", index + 1, manifest.files.len());
                    return Ok(());
                }
            }
            let download_url = match info.data.download_url {
                Some(url) if !url.is_empty() => url,
                _ => {
                    let url = format!("{url}/download-url");
                    let response: Envelope<Option<String>> = api_json(&client, &key, &url).await?;
                    response.data.ok_or_else(|| err("CurseForge did not provide a download URL; this file may require manual download"))?
                }
            };
            println!("[{}/{}] downloading {name}", index + 1, manifest.files.len());
            let temporary = target.with_file_name(format!("{name}.part"));
            if temporary.is_symlink() { return Err(err(format!("symlink in destination: {}", temporary.display()))); }
            let bytes = download(&client, &key, &download_url).await?;
            if bytes.is_empty() { return Err(err("empty download")); }
            if bytes.len() as u64 != info.data.file_length {
                return Err(err(format!("size mismatch: expected {} bytes, got {}", info.data.file_length, bytes.len())));
            }
            sha1_matches(&bytes, &info.data.hashes)?;
            fs::write(&temporary, bytes)?;
            fs::rename(&temporary, &target)?;
            Ok(())
        }.await;
        if let Err(error) = result {
            eprintln!(
                "[{}/{}] {} / {}: {error}",
                index + 1,
                manifest.files.len(),
                file.project_id,
                file.file_id
            );
            if file.required {
                failed.push(format!("{} / {}", file.project_id, file.file_id));
            }
        }
    }
    copy_overrides(&overrides, &destination)?;
    if !failed.is_empty() {
        return Err(err(format!(
            "{} required files failed: {}",
            failed.len(),
            failed.join(", ")
        )));
    }
    println!("Done: {}", destination.display());
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsafe_names() {
        for name in ["../bad.jar", "dir/bad.jar", ".", "..", ""] {
            assert!(valid_component(name).is_err());
        }
        assert!(valid_component("mod-1.0.jar").is_ok());
    }
    #[test]
    fn verifies_sha1() {
        let hashes = [FileHash {
            value: "a9993e364706816aba3e25717850c26c9cd0d89d".into(),
            algo: 1,
        }];
        assert!(sha1_matches(b"abc", &hashes).is_ok());
        assert!(sha1_matches(b"abd", &hashes).is_err());
    }
}
