// Command ukk-relay is a self-hostable control-plane relay. It routes opaque,
// end-to-end encrypted control frames between a host and its paired devices and
// never carries screen media.
package main

import (
	"context"
	"crypto/tls"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/suj1e/ukk/apps/desktop/internal/relay"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintln(os.Stderr, "ukk-relay:", err)
		os.Exit(1)
	}
}

func run() error {
	var (
		addr      = flag.String("addr", "127.0.0.1:8787", "listen address")
		tokenEnv  = flag.String("token-env", "UKK_RELAY_TOKEN", "environment variable holding the operator token")
		tokenFile = flag.String("token-file", "", "file holding the operator token (mode 0600)")
		certFile  = flag.String("tls-cert", "", "TLS certificate chain; with -tls-key serves wss directly")
		keyFile   = flag.String("tls-key", "", "TLS private key")
		showVer   = flag.Bool("version", false, "print the version and exit")
	)
	flag.Parse()
	if *showVer {
		fmt.Println("ukk-relay 0.1.0")
		return nil
	}

	token, err := operatorToken(*tokenEnv, *tokenFile)
	if err != nil {
		return err
	}
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: slog.LevelInfo}))
	server, err := relay.New(relay.Config{
		OperatorToken: token,
		OnEvent: func(event relay.Event) {
			switch event.Kind {
			case relay.EventHostConnected, relay.EventHostDisconnected,
				relay.EventDeviceConnected, relay.EventDeviceDisconnected,
				relay.EventDeviceRevoked, relay.EventRejected, relay.EventQueueOverflow:
				logger.Info("relay", "event", event.Kind, "host", event.HostID,
					"device", event.DeviceID, "code", event.Code)
			}
		},
	})
	if err != nil {
		return err
	}
	httpServer := &http.Server{
		Handler:           server.Handler(),
		ReadHeaderTimeout: 5 * time.Second,
		IdleTimeout:       120 * time.Second,
		MaxHeaderBytes:    64 << 10,
	}
	listener, err := net.Listen("tcp", *addr)
	if err != nil {
		return err
	}
	scheme := "ws"
	if *certFile != "" || *keyFile != "" {
		if *certFile == "" || *keyFile == "" {
			return errors.New("both -tls-cert and -tls-key are required")
		}
		certificate, err := tls.LoadX509KeyPair(*certFile, *keyFile)
		if err != nil {
			return fmt.Errorf("load TLS key pair: %w", err)
		}
		listener = tls.NewListener(listener, &tls.Config{
			Certificates: []tls.Certificate{certificate},
			MinVersion:   tls.VersionTLS12,
		})
		scheme = "wss"
	}
	logger.Info("relay listening", "address", listener.Addr().String(), "scheme", scheme)

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	go func() {
		<-ctx.Done()
		shutdown, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = httpServer.Shutdown(shutdown)
		_ = server.Shutdown(shutdown)
	}()
	err = httpServer.Serve(listener)
	if errors.Is(err, http.ErrServerClosed) {
		return nil
	}
	return err
}

func operatorToken(envName, path string) (string, error) {
	token := strings.TrimSpace(os.Getenv(envName))
	if path != "" {
		info, err := os.Stat(path)
		if err != nil {
			return "", fmt.Errorf("read token file: %w", err)
		}
		if info.Mode().Perm()&0o077 != 0 {
			return "", errors.New("token file must not be readable by group or others")
		}
		raw, err := os.ReadFile(path)
		if err != nil {
			return "", fmt.Errorf("read token file: %w", err)
		}
		token = strings.TrimSpace(string(raw))
	}
	if len(token) < 32 {
		return "", fmt.Errorf("%s must hold an operator token of at least 32 bytes", envName)
	}
	return token, nil
}
