-- Site settings edited on the first-run setup page and in the admin back office.
-- Empty values fall back to the deployment's initial values (SITE_NAME, SITE_URL,
-- APP_TIMEZONE), so an existing site keeps behaving exactly as before.
ALTER TABLE users ADD COLUMN email VARCHAR(254) NULL;

INSERT INTO settings (name, value) VALUES ('site_name', '');
INSERT INTO settings (name, value) VALUES ('site_url', '');
INSERT INTO settings (name, value) VALUES ('timezone', '');

-- A site where someone already changed the seeded password is in use: skip the setup page.
INSERT INTO settings (name, value)
SELECT 'setup_completed', CASE WHEN EXISTS (SELECT 1 FROM users WHERE must_change_password = 0)
    THEN '1' ELSE '0' END;
