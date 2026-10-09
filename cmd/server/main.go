// Command server arranca el proxy con API estilo Claude.
//
// Lee la configuración del .env (o del entorno) y activa los conectores que
// tengan su flag booleano en true: OPENCODE_ZEN_ENABLED, OPENCODE_GO_ENABLED,
// OPENAI_ENABLED.
package main

import (
	"flag"
	"fmt"
	"log/slog"
	"net/http"
	"os"

	"github.com/ruxwez/claude-proxy/internal/config"
	"github.com/ruxwez/claude-proxy/internal/connector"
	openaiconnector "github.com/ruxwez/claude-proxy/internal/connector/openai"
	"github.com/ruxwez/claude-proxy/internal/connector/opencode"
	"github.com/ruxwez/claude-proxy/internal/env"
	"github.com/ruxwez/claude-proxy/internal/server"
)

func main() {
	envPath := flag.String("env", ".env", "ruta al fichero .env (vacío = no cargar)")
	host := flag.String("host", "", "override del host")
	port := flag.Int("port", 0, "override del puerto")
	flag.Parse()

	// .env primero: de ahí salen las API keys y los flags de conectores.
	if err := env.Load(*envPath); err != nil {
		fmt.Fprintln(os.Stderr, "error cargando .env:", err)
		os.Exit(1)
	}

	cfg := config.Load()
	if *host != "" {
		cfg.Host = *host
	}
	if *port != 0 {
		cfg.Port = *port
	}

	log := slog.New(slog.NewTextHandler(os.Stdout, nil))
	reg := buildRegistry(cfg, log)
	if len(reg.Names()) == 0 {
		log.Warn("ningún conector activo: define OPENCODE_ZEN_ENABLED / OPENCODE_GO_ENABLED / OPENAI_ENABLED=true en el .env")
	}

	srv := server.New(cfg, reg, log)
	addr := fmt.Sprintf("%s:%d", cfg.Host, cfg.Port)
	log.Info("claude-proxy escuchando", "addr", addr, "conectores", reg.Names())
	if err := http.ListenAndServe(addr, srv); err != nil {
		log.Error("servidor terminado", "err", err)
		os.Exit(1)
	}
}

// buildRegistry registra los conectores habilitados por el entorno.
func buildRegistry(cfg config.Settings, log *slog.Logger) *connector.Registry {
	reg := connector.NewRegistry()
	if cfg.ZenEnabled {
		if cfg.OpenCodeAPIKey == "" {
			log.Warn("OPENCODE_ZEN_ENABLED=true pero OPENCODE_API_KEY está vacía")
		}
		reg.Add(opencode.NewZen(cfg.OpenCodeAPIKey, cfg.Timeout))
	}
	if cfg.GoEnabled {
		if cfg.OpenCodeAPIKey == "" {
			log.Warn("OPENCODE_GO_ENABLED=true pero OPENCODE_API_KEY está vacía")
		}
		reg.Add(opencode.NewGo(cfg.OpenCodeAPIKey, cfg.Timeout))
	}
	if cfg.OpenAI.Enabled {
		if cfg.OpenAI.APIKey == "" {
			log.Warn("OPENAI_ENABLED=true pero OPENAI_API_KEY está vacía")
		}
		reg.Add(openaiconnector.New(cfg.OpenAI.APIKey, cfg.OpenAI.BaseURL, cfg.OpenAI.Models, cfg.Timeout))
	}
	return reg
}
