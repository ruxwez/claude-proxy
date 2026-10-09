package env

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoad_SetsVars(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, ".env")
	content := "# comentario\nOPENCODE_API_KEY=test-desde-env\nCON_COMILLAS=\"con espacios\"\nexport EXPORTADO=1\n"
	if err := os.WriteFile(p, []byte(content), 0644); err != nil {
		t.Fatal(err)
	}
	os.Unsetenv("OPENCODE_API_KEY")
	os.Unsetenv("CON_COMILLAS")
	os.Unsetenv("EXPORTADO")
	if err := Load(p); err != nil {
		t.Fatal(err)
	}
	if got := os.Getenv("OPENCODE_API_KEY"); got != "test-desde-env" {
		t.Fatalf("OPENCODE_API_KEY = %q", got)
	}
	if got := os.Getenv("CON_COMILLAS"); got != "con espacios" {
		t.Fatalf("CON_COMILLAS = %q", got)
	}
	if got := os.Getenv("EXPORTADO"); got != "1" {
		t.Fatalf("EXPORTADO = %q", got)
	}
}

func TestLoad_NoPisaExistentes(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, ".env")
	if err := os.WriteFile(p, []byte("YA_EXISTO=del-env\n"), 0644); err != nil {
		t.Fatal(err)
	}
	t.Setenv("YA_EXISTO", "del-sistema")
	if err := Load(p); err != nil {
		t.Fatal(err)
	}
	if got := os.Getenv("YA_EXISTO"); got != "del-sistema" {
		t.Fatalf("YA_EXISTO = %q, el .env no debe pisar el entorno", got)
	}
}

func TestLoad_MissingEsNil(t *testing.T) {
	if err := Load(filepath.Join(t.TempDir(), "no-existe.env")); err != nil {
		t.Fatalf("fichero ausente debe ser nil, got %v", err)
	}
	if err := Load(""); err != nil {
		t.Fatalf("path vacío debe ser nil, got %v", err)
	}
}
