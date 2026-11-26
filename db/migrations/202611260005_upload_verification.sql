ALTER TABLE app.files ADD COLUMN sha256 bytea;

ALTER TABLE app.files ADD CONSTRAINT files_sha256_check
    CHECK ((status <> 'ready' OR sha256 IS NOT NULL)
       AND (sha256 IS NULL OR octet_length(sha256) = 32));
