-- 0.5.0: admin sessions of two kinds. `api` sessions are opened by
-- `joedocs login` and used with the admin token on `/admin/v1/`; `web`
-- sessions are opened by the web interface's login page and live in a
-- browser cookie on their own. Neither kind is accepted in place of the
-- other. Sessions from before this migration were all opened by `login`.

ALTER TABLE admin_sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'api'
    CHECK (kind IN ('api', 'web'));
