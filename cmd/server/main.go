// Command server arranca el proxy Claude Like.
package main

import (
	"flag"
	"fmt"
	"log/slog"
	"net/http"
	"os"

	"github.com/ruxwez/claude-proxy/internal/config"
	"github.com/ruxwez/claude-proxy/internal/env"
	"github.com/ruxwez/claude-proxy/internal/server"
)

func main() {
	var (
		cfgPath = flag.String("config", "config.toml", "ruta al fichero .toml")
		envPath = flag.String("env", ".env", "ruta al fichero .env (vacío = no cargar)")
		host    = flag.String("host", "", "override del host (vacío = usar .toml)")
		port    = flag.Int("port", 0, "override del puerto (0 = usar .toml)")
	)
	flag.Parse()

	// .env primero: las keys del .toml (api_key_env) se resuelven del entorno.
	if err := env.Load(*envPath); err != nil {
		fmt.Fprintln(os.Stderr, "error cargando .env:", err)
		os.Exit(1)
	}

	cfg, err := config.Load(*cfgPath)
	if err != nil {
		fmt.Fprintln(os.Stderr, "error cargando config:", err)
		os.Exit(1)
	}
	if *host != "" {
		cfg.Server.Host = *host
	}
	if *port != 0 {
		cfg.Server.Port = *port
	}

	log := slog.New(slog.NewTextHandler(os.Stdout, nil))
	srv := server.New(cfg, log)

	addr := fmt.Sprintf("%s:%d", cfg.Server.Host, cfg.Server.Port)
	log.Info("claude-proxy escuchando",
		"addr", addr,
		"config", *cfgPath,
		"env", *envPath,
		"rutas", len(cfg.Routes),
	)
	if err := http.ListenAndServe(addr, srv); err != nil {
		log.Error("servidor terminado", "err", err)
		os.Exit(1)
	}
}
