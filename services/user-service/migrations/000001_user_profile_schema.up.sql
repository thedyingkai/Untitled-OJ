CREATE TABLE IF NOT EXISTS user_profiles (
    user_id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    bio TEXT NOT NULL DEFAULT '',
    avatar_object TEXT NOT NULL DEFAULT '',
    preferences JSONB NOT NULL DEFAULT '{"theme":"system"}'::jsonb,
    solved_problems INT NOT NULL DEFAULT 0,
    submissions INT NOT NULL DEFAULT 0,
    accepted INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT chk_user_profiles_user_id_not_blank CHECK (length(trim(user_id)) > 0),
    CONSTRAINT chk_user_profiles_solved_non_negative CHECK (solved_problems >= 0),
    CONSTRAINT chk_user_profiles_submissions_non_negative CHECK (submissions >= 0),
    CONSTRAINT chk_user_profiles_accepted_non_negative CHECK (accepted >= 0)
);

CREATE INDEX IF NOT EXISTS idx_user_profiles_display_name
    ON user_profiles(display_name);

CREATE INDEX IF NOT EXISTS idx_user_profiles_updated_at
    ON user_profiles(updated_at);
