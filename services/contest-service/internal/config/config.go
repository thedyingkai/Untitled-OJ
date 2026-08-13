package config

import (
	"errors"
	"os"
	"strings"

	"ojos-shared/resourceoutput"
)

const defaultDatabaseSecret = "/run/ojos/resources/contests/dsn"

type Config struct {
	ListenAddress      string
	DatabaseSecretFile string
	ServiceContextFile string
	RegistrationMode   string
	Managed            bool
}

func Load() (Config, error) {
	managed := envBool("OJOS_MANAGED_WORKLOAD")
	config := Config{
		ListenAddress:      ":8080",
		DatabaseSecretFile: defaultDatabaseSecret,
		ServiceContextFile: envOr("OJOS_SERVICE_CONTEXT_FILE", "/run/ojos/service/context.json"),
		RegistrationMode:   envOr("OJOS_CONFIG_REGISTRATION_MODE", "open"),
		Managed:            managed,
	}
	if value := strings.TrimSpace(os.Getenv("OJOS_RESOURCE_CONTESTS_OUTPUT_FILE")); value != "" {
		config.DatabaseSecretFile = value
	} else if !managed {
		config.DatabaseSecretFile = envOr("CONTEST_DATABASE_SECRET_FILE", defaultDatabaseSecret)
	}
	if !managed {
		config.ListenAddress = envOr("CONTEST_LISTEN_ADDRESS", ":8080")
	}
	if config.ListenAddress == "" {
		return Config{}, errors.New("listen address is required")
	}
	if !containerAbsolutePath(config.DatabaseSecretFile) {
		return Config{}, errors.New("database secret file must be absolute")
	}
	if config.Managed && !containerAbsolutePath(config.ServiceContextFile) {
		return Config{}, errors.New("service context file must be absolute")
	}
	inviteSigningKey := strings.TrimSpace(os.Getenv("OJOS_SECRET_REGISTRATION_INVITESIGNINGKEY"))
	switch config.RegistrationMode {
	case "open":
		if inviteSigningKey != "" {
			return Config{}, errors.New("invite signing key is not allowed in open registration mode")
		}
	case "invite-only":
		if len(inviteSigningKey) < 32 {
			return Config{}, errors.New("invite signing key is required in invite-only registration mode")
		}
	default:
		return Config{}, errors.New("registration mode is invalid")
	}
	return config, nil
}

func containerAbsolutePath(value string) bool {
	return strings.HasPrefix(value, "/") && !strings.ContainsRune(value, '\x00')
}

func ReadDatabaseDSN(path string) (string, error) {
	return resourceoutput.ReadPostgreSQLDSN(path)
}

func envOr(name, fallback string) string {
	if value := strings.TrimSpace(os.Getenv(name)); value != "" {
		return value
	}
	return fallback
}

func envBool(name string) bool {
	value := strings.TrimSpace(os.Getenv(name))
	return value == "1" || strings.EqualFold(value, "true")
}
