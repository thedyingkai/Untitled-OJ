CREATE TABLE users (
                       id BIGSERIAL PRIMARY KEY,

                       username TEXT NOT NULL UNIQUE,
                       email TEXT UNIQUE,

                       password_hash TEXT NOT NULL,

                       created_at TIMESTAMP NOT NULL DEFAULT NOW(),
                       updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE TABLE roles (
                       id BIGSERIAL PRIMARY KEY,

                       name TEXT NOT NULL UNIQUE,
                       description TEXT,

                       created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE TABLE user_roles (
                            user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
                            role_id BIGINT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,

                            PRIMARY KEY(user_id, role_id)
);

CREATE INDEX idx_user_roles_user_id ON user_roles(user_id);
CREATE INDEX idx_user_roles_role_id ON user_roles(role_id);

INSERT INTO roles(name, description)
VALUES
    ('super_admin', 'system super administrator'),
    ('admin', 'administrator'),
    ('user', 'normal user');