ALTER TABLE app.files DROP CONSTRAINT files_status_check;

ALTER TABLE app.files ADD CONSTRAINT files_status_check
    CHECK (status IN ('pending', 'uploading', 'verifying', 'ready', 'failed', 'deleting', 'cancelled'));

ALTER TABLE app.uploads ADD COLUMN creation_claimed boolean NOT NULL DEFAULT false;
