-- Sessions are now looked up by the SHA-256 of the cookie token, so a leaked database
-- or backup no longer contains usable sessions. Old rows hold raw tokens: sign everyone out.
DELETE FROM sessions;

-- Repeated wrong passwords lock the account for a while (see store::record_failed_login).
ALTER TABLE users ADD COLUMN failed_logins BIGINT NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN locked_until BIGINT NULL;
