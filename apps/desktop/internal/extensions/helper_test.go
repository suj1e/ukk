package extensions

import (
	"bytes"
	"encoding/binary"
	"encoding/json"
	"os"
	"testing"
	"time"
)

// TestHelperProcess is the fake helper. It is only active when the supervisor
// spawns a copy of the test binary with GO_EXTENSIONS_HELPER=1; in the ordinary
// test run it returns immediately.
func TestHelperProcess(t *testing.T) {
	if os.Getenv("GO_EXTENSIONS_HELPER") != "1" {
		return
	}
	serveHelper()
	os.Exit(0)
}

func serveHelper() {
	mode := os.Getenv("GO_EXTENSIONS_HELPER_MODE")
	for {
		payload, err := ReadFrame(os.Stdin)
		if err != nil {
			return
		}
		var request Request
		if err := json.Unmarshal(payload, &request); err != nil {
			os.Exit(2)
		}
		switch request.Method {
		case MethodHello:
			result, err := json.Marshal(HelloResult{PluginID: request.PluginID, Protocol: ProtocolVersion})
			if err != nil {
				os.Exit(2)
			}
			if err := WriteFrame(os.Stdout, Response{ID: request.ID, OK: true, Result: result}); err != nil {
				return
			}
		case MethodShutdown:
			return
		case MethodInvoke:
			switch mode {
			case "hang":
				time.Sleep(10 * time.Second)
				return
			case "oversize":
				var header [4]byte
				binary.BigEndian.PutUint32(header[:], uint32(MaxFrameBytes+1024))
				if _, err := os.Stdout.Write(header[:]); err != nil {
					return
				}
				if _, err := os.Stdout.Write(bytes.Repeat([]byte("x"), 1024)); err != nil {
					return
				}
				time.Sleep(10 * time.Second)
				return
			case "error":
				if err := WriteFrame(os.Stdout, Response{
					ID:    request.ID,
					OK:    false,
					Error: &WireError{Code: "refused", Message: "not today"},
				}); err != nil {
					return
				}
			default:
				result, err := json.Marshal(map[string]any{
					"pong":    true,
					"command": request.Command,
					"session": request.Session,
					"mode":    mode,
					"leak":    os.Getenv("EXT_LEAK"),
				})
				if err != nil {
					os.Exit(2)
				}
				if err := WriteFrame(os.Stdout, Response{ID: request.ID, OK: true, Result: result}); err != nil {
					return
				}
			}
		default:
			if err := WriteFrame(os.Stdout, Response{
				ID:    request.ID,
				OK:    false,
				Error: &WireError{Code: "unknown_method", Message: request.Method},
			}); err != nil {
				return
			}
		}
	}
}
