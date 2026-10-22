// Records how Go v8.0.35's config decoder reads !!binary and locally tagged
// scalars, for
// crates/cpa-core/tests/binary_strings_go.rs. yaml.v3 base64-decodes a scalar whose
// tag resolves to !!binary into any string field. Run from a module nested under the
// v8 module path (so internal/config is importable) with a replace to the reference
// clone:
//
//	go run . ../../fixtures/binary_strings_go.json
//
// No files and no network: each case is parsed from memory.
//
// A !!binary mapping key is left out: Go's layout code matches keys by their
// written text, and cliproxy-rs refuses such a key.
package main

import (
	"encoding/json"
	"os"
	"unicode/utf8"

	"github.com/router-for-me/CLIProxyAPI/v8/internal/config"
	"gopkg.in/yaml.v3"
)

type result struct {
	Name string `json:"name"`
	YAML string `json:"yaml"`
	// Error is Go's error text, empty on success.
	Error string `json:"error"`
	// SecretKey is the decoded management key; SecretBytes holds it as base64
	// instead when it is not UTF-8.
	SecretKey   string   `json:"secret_key"`
	SecretBytes []byte   `json:"secret_bytes,omitempty"`
	ProxyURL    string   `json:"proxy_url"`
	Port        int      `json:"port"`
	APIKeys     []string `json:"api_keys"`
}

func main() {
	type tc struct{ name, yaml string }
	cases := []tc{
		{"plain secret", "management:\n  secret-key: secret\n"},
		{"binary v8 secret", "management:\n  secret-key: !!binary c2VjcmV0\n"},
		{"binary legacy secret", "remote-management:\n  secret-key: !!binary c2VjcmV0\n"},
		{"quoted binary secret", "management:\n  secret-key: !!binary 'c2VjcmV0'\n"},
		{"block binary secret", "management:\n  secret-key: !!binary |\n    c2Vj\n    cmV0\n"},
		{"verbatim binary secret", "management:\n  secret-key: !<tag:yaml.org,2002:binary> c2VjcmV0\n"},
		{"escaped binary secret", "management:\n  secret-key: !!bin%61ry c2VjcmV0\n"},
		{"short-form binary secret", "management:\n  secret-key: !<!!binary> c2VjcmV0\n"},
		{"escaped primary binary secret", "management:\n  secret-key: !%21binary c2VjcmV0\n"},
		{"named handle binary secret", "%TAG !e! tag:yaml.org,2002:\n---\nmanagement:\n  secret-key: !e!binary c2VjcmV0\n"},
		{"redefined secondary handle secret", "%TAG !! tag:example.com,2000:\n---\nmanagement:\n  secret-key: !!binary c2VjcmV0\n"},
		{"custom-tagged secret", "management:\n  secret-key: !keep c2VjcmV0\n"},
		{"non-specific tagged secret", "management:\n  secret-key: ! secret\n"},
		{"custom-tagged number secret", "management:\n  secret-key: !keep 0x1F\n"},
		{"custom-tagged port", "port: !keep 8317\n"},
		{"aliased custom-tagged secret", "proxy-url: &a !keep secret\nmanagement:\n  secret-key: *a\n"},
		{"binary proxy-url", "proxy-url: !!binary aHR0cDovL3Byb3h5Lmxhbjo4MA==\n"},
		{"binary api key", "api-keys: [plain, !!binary a2V5]\n"},
		{"aliased binary secret", "proxy-url: &a !!binary c2VjcmV0\nmanagement:\n  secret-key: *a\n"},
		{"binary secret through a merge", "defaults: &d\n  secret-key: !!binary c2VjcmV0\nmanagement:\n  <<: *d\n"},
		{"binary secret through an aliased section", "defaults: &d\n  secret-key: !!binary c2VjcmV0\nmanagement: *d\n"},
		{"unused bits", "management:\n  secret-key: !!binary YR==\n"},
		{"invalid base64", "management:\n  secret-key: !!binary 'c2VjcmV0!'\n"},
		{"missing padding", "management:\n  secret-key: !!binary YQ\n"},
		{"space in base64", "management:\n  secret-key: !!binary 'c2Vj cmV0'\n"},
		{"invalid base64 in an unknown setting", "extra: !!binary '!!!'\n"},
		{"binary unknown setting", "extra: !!binary c2VjcmV0\n"},
		{"not UTF-8", "management:\n  secret-key: !!binary /w==\n"},
		{"binary port", "port: !!binary ODMxNw==\n"},
		{"binary boolean", "debug: !!binary dHJ1ZQ==\n"},
	}
	var out []result
	for _, c := range cases {
		r := result{Name: c.name, YAML: c.yaml}
		// ParseConfigBytes also validates and hashes the secret; the plain decode
		// shows the values as read.
		var cfg config.Config
		if _, err := config.ParseConfigBytes([]byte(c.yaml)); err != nil {
			r.Error = err.Error()
		} else if err := yaml.Unmarshal([]byte(c.yaml), &cfg); err != nil {
			panic(err)
		} else {
			r.SecretKey = cfg.RemoteManagement.SecretKey
			if !utf8.ValidString(r.SecretKey) {
				r.SecretBytes, r.SecretKey = []byte(r.SecretKey), ""
			}
			r.ProxyURL, r.Port, r.APIKeys = cfg.ProxyURL, cfg.Port, cfg.APIKeys
		}
		out = append(out, r)
	}
	data, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		panic(err)
	}
	if err := os.WriteFile(os.Args[1], append(data, '\n'), 0o644); err != nil {
		panic(err)
	}
}
