CREATE TABLE app.users (
    id uuid PRIMARY KEY,
    legacy_user_id text UNIQUE,
    display_name text NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 100),
    active boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE app.oauth_accounts (
    provider text NOT NULL,
    issuer text NOT NULL,
    subject text NOT NULL,
    user_id uuid NOT NULL REFERENCES app.users(id) ON DELETE CASCADE,
    PRIMARY KEY (provider, issuer, subject),
    UNIQUE (user_id, provider, issuer)
);

CREATE TABLE app.oauth_attempts (
    state_hash bytea PRIMARY KEY,
    browser_hash bytea NOT NULL,
    pkce_verifier text NOT NULL,
    return_to text NOT NULL,
    expires_at timestamptz NOT NULL
);

CREATE INDEX oauth_attempts_expiry_idx ON app.oauth_attempts(expires_at);

CREATE TABLE app.sessions (
    token_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES app.users(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz
);

CREATE INDEX sessions_expiry_idx ON app.sessions(expires_at);

CREATE TABLE app.memos (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES app.users(id) ON DELETE CASCADE,
    create_key uuid NOT NULL,
    title text NOT NULL CHECK (char_length(title) <= 200),
    body text NOT NULL CHECK (char_length(body) <= 100000),
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (owner_id, create_key)
);

CREATE INDEX memos_owner_order_idx ON app.memos(owner_id, updated_at DESC, id DESC);
