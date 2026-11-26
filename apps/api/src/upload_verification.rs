use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{database_pool, shutdown};

const SCAN_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) async fn observe(db: &PgPool, id: Uuid, size: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE app.files f SET status = 'verifying' FROM app.uploads u
         WHERE u.file_id = f.id AND u.id = $1 AND u.creation_claimed
           AND f.status IN ('pending', 'uploading') AND f.size_bytes = $2",
    )
    .bind(id)
    .bind(size)
    .execute(db)
    .await?;
    Ok(())
}

pub(crate) async fn run() -> Result<(), &'static str> {
    let db = database_pool("DATABASE_URL", 2)?;
    let root = std::env::var_os("TUS_UPLOAD_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or("TUS_UPLOAD_DIR must be an absolute path to the tusd volume")?;

    tokio::select! {
        _ = shutdown() => {}
        _ = async {
            loop {
                if let Err(error) = scan(&db, &root).await {
                    tracing::warn!(%error, "upload verification scan failed");
                }
                tokio::time::sleep(SCAN_INTERVAL).await;
            }
        } => {}
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Candidate {
    id: Uuid,
    size_bytes: i64,
    verifying: bool,
}

async fn scan(db: &PgPool, root: &Path) -> Result<(), sqlx::Error> {
    let mut cursor = Uuid::nil();
    loop {
        let batch = sqlx::query_as::<_, Candidate>(
            "SELECT u.id, f.size_bytes, f.status = 'verifying' AS verifying
             FROM app.uploads u JOIN app.files f ON f.id = u.file_id
             WHERE u.id > $1 AND u.creation_claimed
               AND f.status IN ('pending', 'uploading', 'verifying')
             ORDER BY u.id LIMIT 64",
        )
        .bind(cursor)
        .fetch_all(db)
        .await?;
        if batch.is_empty() {
            return Ok(());
        }

        for candidate in batch {
            cursor = candidate.id;
            if !candidate.verifying {
                let path = root.join(candidate.id.to_string());
                let id = candidate.id;
                let size = candidate.size_bytes;
                match tokio::task::spawn_blocking(move || stored_complete(&path, id, size))
                    .await
                    .expect("upload inspection task did not panic")
                {
                    Ok(true) => observe(db, id, size).await?,
                    Ok(false) => continue,
                    Err(error) => {
                        tracing::warn!(upload_id = %id, %error, "could not inspect tusd upload");
                        continue;
                    }
                }
            }
            verify(db, root, candidate).await?;
        }
    }
}

async fn verify(db: &PgPool, root: &Path, candidate: Candidate) -> Result<(), sqlx::Error> {
    let id = candidate.id;
    let path = root.join(id.to_string());
    let verification = tokio::task::spawn_blocking(move || hash_file(&path, candidate.size_bytes))
        .await
        .expect("upload hashing task did not panic");
    let (status, sha256) = match verification {
        Ok(Some(digest)) => ("ready", Some(digest)),
        Ok(None) => {
            tracing::warn!(upload_id = %id, "uploaded file type or size does not match");
            ("failed", None)
        }
        Err(error) => {
            // The volume can disappear temporarily. Keep the job for the next scan.
            tracing::warn!(upload_id = %id, %error, "could not read uploaded file");
            return Ok(());
        }
    };

    sqlx::query(
        "UPDATE app.files f SET status = $2, sha256 = $3 FROM app.uploads u
         WHERE u.file_id = f.id AND u.id = $1 AND f.status = 'verifying'",
    )
    .bind(id)
    .bind(status)
    .bind(sha256.as_ref().map(|digest| digest.as_slice()))
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct FileStoreInfo {
    #[serde(rename = "ID")]
    id: Uuid,
    size: i64,
    size_is_deferred: bool,
}

// The pinned tusd FileStore derives its offset from the payload length. Read
// the same evidence from disk because a tusd HEAD can interrupt an active PATCH.
fn stored_complete(path: &Path, id: Uuid, size: i64) -> io::Result<bool> {
    let result: io::Result<bool> = (|| {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.len() != size as u64 {
            return Ok(false);
        }

        let mut bytes = Vec::new();
        File::open(path.with_extension("info"))?
            .take(8192)
            .read_to_end(&mut bytes)?;
        let info: FileStoreInfo = serde_json::from_slice(&bytes)?;
        Ok(info.id == id && info.size == size && !info.size_is_deferred)
    })();
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        other => other,
    }
}

fn hash_file(path: &Path, expected_size: i64) -> io::Result<Option<[u8; 32]>> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Ok(None);
    }
    let file = File::open(path)?;
    if file.metadata()?.len() != expected_size as u64 {
        return Ok(None);
    }

    let mut reader = file.take(expected_size as u64 + 1);
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok((reader.limit() == 1).then(|| hasher.finalize().into()))
}
