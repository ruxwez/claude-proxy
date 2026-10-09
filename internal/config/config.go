// Package config carga el .toml y resuelve qué proveedor/modelo
// corresponde a cada modelo Anthropic solicitado por el cliente.
package config

import (
	"fmt"
	"os"
	"path"
	"strings"

	"github.com/BurntSushi/toml"
)

// Config es el contenido del .toml.
type Config struct {
	Server  ServerConfig `toml:"server"`
	Default *Route       `toml:"default"`
	Routes  []Route      `toml:"routes"`
}

type ServerConfig struct {
	Host      string `toml:"host"`
	Port      int    `toml:"port"`
	AuthToken string `toml:"auth_token"`
	LogLevel  string `toml:"log_level"`
}

// Route es una entrada [[routes]] (o [default] sin anthropic_model).
type Route struct {
	AnthropicModel string            `toml:"anthropic_model"`
	Provider       string            `toml:"provider"`
	TargetModel    string            `toml:"target_model"`
	BaseURL        string            `toml:"base_url"`
	APIKey         string            `toml:"api_key"`
	APIKeyEnv      string            `toml:"api_key_env"`
	TimeoutSeconds int               `toml:"timeout_seconds"`
	ExtraHeaders   map[string]string `toml:"extra_headers"`
}

// ResolvedRoute es una ruta con valores finales (key resuelta, defaults).
type ResolvedRoute struct {
	AnthropicModel string
	Provider       string
	TargetModel    string
	BaseURL        string
	APIKey         string
	TimeoutSeconds int
	ExtraHeaders   map[string]string
}

// Load lee y valida el fichero TOML.
func Load(path string) (*Config, error) {
	var cfg Config
	if _, err := toml.DecodeFile(path, &cfg); err != nil {
		return nil, fmt.Errorf("leer config %s: %w", path, err)
	}
	if cfg.Server.Port == 0 {
		cfg.Server.Port = 8080
	}
	if cfg.Server.Host == "" {
		cfg.Server.Host = "0.0.0.0"
	}
	for i := range cfg.Routes {
		if err := normalize(&cfg.Routes[i]); err != nil {
			return nil, fmt.Errorf("routes[%d]: %w", i, err)
		}
	}
	if cfg.Default != nil {
		if err := normalize(cfg.Default); err != nil {
			return nil, fmt.Errorf("default: %w", err)
		}
		if cfg.Default.Provider == "" {
			return nil, fmt.Errorf("default: provider vacío")
		}
		if cfg.Default.TargetModel == "" {
			return nil, fmt.Errorf("default: target_model vacío")
		}
	}
	return &cfg, nil
}

func normalize(r *Route) error {
	r.Provider = strings.ToLower(strings.TrimSpace(r.Provider))
	r.BaseURL = strings.TrimRight(strings.TrimSpace(r.BaseURL), "/")
	if r.TimeoutSeconds <= 0 {
		r.TimeoutSeconds = 120
	}
	if r.Provider == "" {
		return fmt.Errorf("provider vacío")
	}
	switch r.Provider {
	case "openai":
		if r.BaseURL == "" {
			r.BaseURL = "https://api.openai.com/v1"
		}
	case "anthropic":
		if r.BaseURL == "" {
			r.BaseURL = "https://api.anthropic.com/v1"
		}
	case "opencode", "opencode-responses":
		if r.BaseURL == "" {
			r.BaseURL = "https://opencode.ai/zen/v1"
		}
	}
	if r.TargetModel == "" {
		return fmt.Errorf("target_model vacío")
	}
	return nil
}

// Resolve busca la ruta para un modelo Anthropic:
// 1) coincidencia exacta, 2) comodín path.Match, 3) default.
// Si target_model es "*", se usa el modelo solicitado tal cual
// (útil para flotas como los *-free de OpenCode Zen).
func (c *Config) Resolve(model string) (ResolvedRoute, error) {
	for _, r := range c.Routes {
		if r.AnthropicModel == model {
			return c.resolveWith(model, r), nil
		}
	}
	for _, r := range c.Routes {
		pat := r.AnthropicModel
		if pat == "" || !strings.Contains(pat, "*") {
			continue
		}
		ok, err := path.Match(pat, model)
		if err == nil && ok {
			return c.resolveWith(model, r), nil
		}
	}
	if c.Default != nil {
		return c.resolveWith(model, *c.Default), nil
	}
	return ResolvedRoute{}, fmt.Errorf("modelo %q no configurado y sin [default]", model)
}

func (c *Config) resolveWith(model string, r Route) ResolvedRoute {
	rr := c.resolve(r)
	if rr.TargetModel == "*" {
		rr.TargetModel = model
	}
	return rr
}

func (c *Config) resolve(r Route) ResolvedRoute {
	return ResolvedRoute{
		AnthropicModel: r.AnthropicModel,
		Provider:       r.Provider,
		TargetModel:    r.TargetModel,
		BaseURL:        r.BaseURL,
		APIKey:         resolveKey(r),
		TimeoutSeconds: r.TimeoutSeconds,
		ExtraHeaders:   r.ExtraHeaders,
	}
}

func resolveKey(r Route) string {
	if r.APIKey != "" {
		return os.ExpandEnv(r.APIKey)
	}
	if r.APIKeyEnv != "" {
		return os.Getenv(r.APIKeyEnv)
	}
	return ""
}

// Models lista los anthropic_model configurados (para GET /v1/models).
func (c *Config) Models() []string {
	out := make([]string, 0, len(c.Routes))
	for _, r := range c.Routes {
		if r.AnthropicModel != "" {
			out = append(out, r.AnthropicModel)
		}
	}
	return out
}
