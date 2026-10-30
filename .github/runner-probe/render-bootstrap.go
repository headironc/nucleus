// Test adapter for the text/template renderer used by garm-provider-common.
// This is not a deployment command and accepts only synthetic test inputs.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"text/template"
)

type context struct {
	UseJITConfig    bool
	EnableBootDebug bool
	MetadataURL     string
	CallbackURL     string
	RunnerUsername  string
	CallbackToken   string
}

func main() {
	if err := render(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func render() error {
	if len(os.Args) != 2 {
		return fmt.Errorf("usage: render-bootstrap TEMPLATE")
	}
	var input context
	decoder := json.NewDecoder(os.Stdin)
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&input); err != nil {
		return fmt.Errorf("decode synthetic context: %w", err)
	}
	source, err := os.ReadFile(os.Args[1])
	if err != nil {
		return err
	}
	parsed, err := template.New("").Parse(string(source))
	if err != nil {
		return err
	}
	return parsed.Execute(os.Stdout, input)
}
