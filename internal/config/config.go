// Package config construye la configuración del servidor a partir de las
// variables de entorno (normalmente cargadas desde un fichero .env).
//
// No hay fichero TOML: cada conector se activa con un flag booleano.
package config

import (
	"strings"
	"time"

	"github.com/ruxwez/claude-proxy/internal/env"
)

// Base URLs por defecto de los proveedores.
const (
	ZenBaseURL    = "https://opencode.ai/zen/v1"
	GoBaseURL     = "https://opencode.ai/zen/go/v1"
	OpenAIBaseURL = "https://api.openai.com/v1"
)

// Settings es la configuración completa del servidor.
type Settings struct {
	Host      string        // PROXY_HOST (def. 0.0.0.0)
	Port      int           // PROXY_PORT (def. 8080)
	AuthToken string        // PROXY_AUTH_TOKEN (vacío = sin auth)
	Timeout   time.Duration // REQUEST_TIMEOUT_SECONDS (def. 120s)

	// OpenCode: Zen y Go comparten la misma API key.
	OpenCodeAPIKey string // OPENCODE_API_KEY
	ZenEnabled     bool   // OPENCODE_ZEN_ENABLED
	GoEnabled      bool   // OPENCODE_GO_ENABLED

	// Conector OpenAI genérico.
	OpenAI OpenAI
}

// OpenAI agrupa la configuración del conector OpenAI genérico.
type OpenAI struct {
	Enabled bool     // OPENAI_ENABLED
	APIKey  string   // OPENAI_API_KEY
	BaseURL string   // OPENAI_BASE_URL (def. https://api.openai.com/v1)
	Models  []string // OPENAI_MODELS (opcional, CSV; evita consultar /models)
}

// Load lee la configuración del entorno y aplica los valores por defecto.
func Load() Settings {
	s := Settings{
		Host:      env.Get("PROXY_HOST", "0.0.0.0"),
		Port:      env.Int("PROXY_PORT", 8080),
		AuthToken: env.Get("PROXY_AUTH_TOKEN", ""),
		Timeout:   time.Duration(env.Int("REQUEST_TIMEOUT_SECONDS", 120)) * time.Second,

		OpenCodeAPIKey: env.Get("OPENCODE_API_KEY", ""),
		ZenEnabled:     env.Bool("OPENCODE_ZEN_ENABLED", false),
		GoEnabled:      env.Bool("OPENCODE_GO_ENABLED", false),

		OpenAI: OpenAI{
			Enabled: env.Bool("OPENAI_ENABLED", false),
			APIKey:  env.Get("OPENAI_API_KEY", ""),
			BaseURL: strings.TrimRight(env.Get("OPENAI_BASE_URL", OpenAIBaseURL), "/"),
			Models:  env.CSV("OPENAI_MODELS"),
		},
	}
	if s.Timeout <= 0 {
		s.Timeout = 120 * time.Second
	}
	return s
}
