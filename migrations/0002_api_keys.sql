-- Users may hold several API keys. Revoked keys stay so their past usage
-- still counts toward their owner.
CREATE TABLE api_keys (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  label TEXT NOT NULL,
  ciphertext BLOB NOT NULL,
  nonce BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER
);

CREATE INDEX idx_api_keys_user_id ON api_keys(user_id);

INSERT INTO api_keys (user_id, label, ciphertext, nonce, created_at)
SELECT id, 'Default', api_key_ciphertext, api_key_nonce, created_at FROM users;

ALTER TABLE users DROP COLUMN api_key_ciphertext;
ALTER TABLE users DROP COLUMN api_key_nonce;
