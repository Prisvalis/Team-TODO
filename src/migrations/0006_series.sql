-- Recurring events: a series generates one issue per occurrence, a few weeks ahead.

CREATE TABLE series (
    id              CHAR(26)     NOT NULL PRIMARY KEY,
    title           VARCHAR(200) NOT NULL,
    body            TEXT         NOT NULL,
    visibility      VARCHAR(16)  NOT NULL CHECK (visibility IN ('private', 'public')),
    issuer_id       CHAR(26)     NOT NULL,
    assignee_id     CHAR(26)     NULL,
    freq            VARCHAR(16)  NOT NULL CHECK (freq IN ('daily', 'weekly', 'monthly')),
    -- Weekly only: bit 0 = Monday … bit 6 = Sunday.
    weekdays        INT          NOT NULL DEFAULT 0,
    -- Local dates (YYYY-MM-DD) and times in `timezone`.
    start_date      CHAR(10)     NOT NULL,
    until_date      CHAR(10)     NULL,
    start_minute    INT          NOT NULL,
    duration_min    INT          NOT NULL,
    timezone        VARCHAR(64)  NOT NULL,
    -- Occurrences starting at or before this time (creation or last edit) are never generated.
    effective_from  BIGINT       NOT NULL,
    -- Last local date already turned into issues; NULL until the first run or after an edit.
    generated_until CHAR(10)     NULL,
    stopped_at      BIGINT       NULL,
    created_at      BIGINT       NOT NULL,
    updated_at      BIGINT       NOT NULL,
    FOREIGN KEY (issuer_id) REFERENCES users (id),
    FOREIGN KEY (assignee_id) REFERENCES users (id)
);

CREATE INDEX idx_series_pending ON series (stopped_at, generated_until);

CREATE TABLE series_labels (
    series_id CHAR(26) NOT NULL,
    label_id  CHAR(26) NOT NULL,
    PRIMARY KEY (series_id, label_id),
    FOREIGN KEY (series_id) REFERENCES series (id) ON DELETE CASCADE,
    FOREIGN KEY (label_id) REFERENCES labels (id) ON DELETE CASCADE
);

-- Personal reminders copied onto every generated occurrence.
CREATE TABLE series_reminders (
    series_id  CHAR(26)    NOT NULL,
    user_id    CHAR(26)    NOT NULL,
    mode       VARCHAR(16) NOT NULL CHECK (mode IN ('before_start', 'before_due')),
    offset_min BIGINT      NOT NULL,
    PRIMARY KEY (series_id, user_id),
    FOREIGN KEY (series_id) REFERENCES series (id) ON DELETE CASCADE,
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE
);

ALTER TABLE issues ADD COLUMN series_id CHAR(26) NULL;
ALTER TABLE issues ADD COLUMN occurrence CHAR(10) NULL;

-- One issue per occurrence, even when two requests generate at the same time.
CREATE UNIQUE INDEX idx_issues_occurrence ON issues (series_id, occurrence);
