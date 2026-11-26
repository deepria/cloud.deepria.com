CREATE TABLE app.files (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES app.users(id) ON DELETE CASCADE,
    reservation_key uuid NOT NULL,
    original_name text NOT NULL CHECK (char_length(original_name) BETWEEN 1 AND 255),
    size_bytes bigint NOT NULL CHECK (size_bytes BETWEEN 0 AND 10737418240),
    status text NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'uploading', 'verifying', 'ready', 'failed', 'deleting')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_id, reservation_key)
);

CREATE TABLE app.uploads (
    id uuid PRIMARY KEY,
    file_id uuid NOT NULL UNIQUE REFERENCES app.files(id) ON DELETE CASCADE
);
