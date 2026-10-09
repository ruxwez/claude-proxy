// Package env carga variables de entorno desde un fichero .env.
//
// Se usa al arrancar el servidor para que las keys referenciadas en el .toml
// (api_key_env, api_key con $VAR) puedan definirse en un .env local en vez
// de exportarlas a mano. Nunca pisa variables ya existentes en el entorno.
package env

import (
	"errors"
	"os"

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
