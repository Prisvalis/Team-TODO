-- Reminders can also fire before an issue's start time. SQLite cannot change a CHECK
-- constraint in place, so the table is rebuilt the same way on both databases.
CREATE TABLE reminders_new (
    issue_id    CHAR(26)    NOT NULL,
    user_id     CHAR(26)    NOT NULL,
    mode        VARCHAR(16) NOT NULL CHECK (mode IN ('exact', 'before_due', 'before_start')),
    offset_min  BIGINT      NULL,
    remind_at   BIGINT      NOT NULL,
    notified_at BIGINT      NULL,
    PRIMARY KEY (issue_id, user_id),
    FOREIGN KEY (issue_id) REFERENCES issues (id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

INSERT INTO reminders_new (issue_id, user_id, mode, offset_min, remind_at, notified_at)
SELECT issue_id, user_id, mode, offset_min, remind_at, notified_at FROM reminders;

DROP TABLE reminders;
ALTER TABLE reminders_new RENAME TO reminders;
CREATE INDEX idx_reminders_due ON reminders (user_id, notified_at, remind_at);
