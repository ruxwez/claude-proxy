// Package env carga variables de entorno desde un fichero .env y ofrece
// helpers tipados para leerlas con valores por defecto.
//
// El servidor lee de aquí las API keys y los flags booleanos que activan
// cada conector. Nunca pisa variables ya presentes en el entorno del sistema.
package env

import (
	"errors"
	"os"
	"strconv"
	"strings"

	"github.com/joho/godotenv"
)

// Load carga path (por defecto ".env") al entorno.
// Si el fichero no existe, no hace nada y devuelve nil.
// Si path es "", no hace nada.
func Load(path string) error {
	if path == "" {
		return nil
	}
	if _, err := os.Stat(path); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return err
	}
	return godotenv.Load(path)
}

// Get devuelve la variable recortada o def si está vacía o sin definir.
func Get(key, def string) string {
	if v, ok := os.LookupEnv(key); ok {
		if v = strings.TrimSpace(v); v != "" {
			return v
		}
	}
	return def
}

// Bool interpreta la variable como booleano. Acepta true/false, 1/0,
// yes/no, on/off, si/no (case-insensitive). Si falta o no es válida, def.
func Bool(key string, def bool) bool {
	v, ok := os.LookupEnv(key)
	if !ok {
		return def
	}
	switch strings.ToLower(strings.TrimSpace(v)) {
	case "1", "true", "yes", "on", "si", "sí":
		return true
	case "0", "false", "no", "off":
		return false
	default:
		return def
	}
}

// Int interpreta la variable como entero. Si falta o no es válida, def.
func Int(key string, def int) int {
	v, ok := os.LookupEnv(key)
	if !ok {
		return def
	}
	n, err := strconv.Atoi(strings.TrimSpace(v))
	if err != nil {
		return def
	}
	return n
}

// CSV devuelve una lista separada por comas, sin espacios y sin vacíos.
func CSV(key string) []string {
	v := Get(key, "")
	if v == "" {
		return nil
	}
	parts := strings.Split(v, ",")
	out := make([]string, 0, len(parts))
	for _, p := range parts {
		if p = strings.TrimSpace(p); p != "" {
			out = append(out, p)
		}
	}
	return out
}
