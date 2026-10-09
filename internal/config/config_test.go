package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestResolve_ExactWildcardDefault(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "c.toml")
	content := `
[server]
port = 8080

[default]
provider = "openai"
target_model = "gpt-4o-mini"

[[routes]]
anthropic_model = "claude-3-5-sonnet-20241022"
provider = "openai"
target_model = "gpt-4o-mini"
base_url = "https://api.openai.com/v1"

[[routes]]
anthropic_model = "claude-3-opus-*"
provider = "openai"
target_model = "gpt-4o"
`
	if err := os.WriteFile(p, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	cfg, err := Load(p)
	if err != nil {
		t.Fatal(err)
	}
	r, err := cfg.Resolve("claude-3-5-sonnet-20241022")
	if err != nil || r.TargetModel != "gpt-4o-mini" {
		t.Fatalf("exacta: %+v %v", r, err)
	}
	r, err = cfg.Resolve("claude-3-opus-20240229")
	if err != nil || r.TargetModel != "gpt-4o" {
		t.Fatalf("comodín: %+v %v", r, err)
	}
	r, err = cfg.Resolve("otro-modelo")
	if err != nil || r.TargetModel != "gpt-4o-mini" {
		t.Fatalf("default: %+v %v", r, err)
	}
}

func TestResolve_NoMatchNoDefault(t *testing.T) {
	cfg := &Config{}
	if _, err := cfg.Resolve("x"); err == nil {
		t.Fatal("esperaba error sin rutas ni default")
	}
}

func TestResolve_TargetStarPassthrough(t *testing.T) {
	cfg := &Config{
		Routes: []Route{{
			AnthropicModel: "*-free",
			Provider:       "opencode",
			TargetModel:    "*",
			BaseURL:        "https://opencode.ai/zen/v1",
		}},
	}
	got, err := cfg.Resolve("exo-free")
	if err != nil {
		t.Fatal(err)
	}
	if got.TargetModel != "exo-free" {
		t.Fatalf("target = %q, want exo-free", got.TargetModel)
	}
	if got.BaseURL != "https://opencode.ai/zen/v1" {
		t.Fatalf("base = %q", got.BaseURL)
	}
}

func TestLoad_OpencodeExample(t *testing.T) {
	cfg, err := Load("../../config.opencode.toml")
	if err != nil {
		t.Fatal(err)
	}
	for _, m := range []string{"exo-free", "muse-spark-1.3-contributor-free", "big-pickle"} {
		r, err := cfg.Resolve(m)
		if err != nil {
			t.Fatalf("Resolve(%q): %v", m, err)
		}
		if r.TargetModel != m {
			t.Fatalf("Resolve(%q).Target = %q", m, r.TargetModel)
		}
	}
	// muse-spark debe ir por responses, el resto por chat.
	r, _ := cfg.Resolve("muse-spark-1.3-contributor-free")
	if r.Provider != "opencode-responses" {
		t.Fatalf("provider muse-spark = %q", r.Provider)
	}
	r, _ = cfg.Resolve("exo-free")
	if r.Provider != "opencode" {
		t.Fatalf("provider exo = %q", r.Provider)
	}
	// Futuro modelo free cae en el comodín con passthrough.
	r, err = cfg.Resolve("algun-modelo-free")
	if err != nil || r.TargetModel != "algun-modelo-free" {
		t.Fatalf("comodín free: %+v %v", r, err)
	}
}

func TestResolveKey(t *testing.T) {
	os.Setenv("TEST_CLAUDE_PROXY_KEY", "sk-test")
	defer os.Unsetenv("TEST_CLAUDE_PROXY_KEY")
	r := Route{APIKeyEnv: "TEST_CLAUDE_PROXY_KEY", TimeoutSeconds: 5}
	// normalize no debe romper
	_ = r
	cfg := &Config{Default: &Route{Provider: "openai", TargetModel: "gpt-x", APIKeyEnv: "TEST_CLAUDE_PROXY_KEY"}}
	got, err := cfg.Resolve("cualquiera")
	if err != nil {
		t.Fatal(err)
	}
	if got.APIKey != "sk-test" {
		t.Fatalf("api key = %q", got.APIKey)
	}
}
