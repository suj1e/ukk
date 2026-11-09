package hc

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"
)

func TestDocRequestsCarryTheDocumentParams(t *testing.T) {
	client, logPath := startFake(t, "")
	defer client.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	value := json.RawMessage(`{"spaces":[{"id":"s","name":"n","folder":"","tabs":[]}]}`)

	// The fake core does not implement doc methods, so both calls fail with
	// the RPC error; the request is still logged with its exact params.
	if _, _, err := client.DocGet(ctx, "herdr-app", ""); !IsCode(err, CodeRPC) {
		t.Fatalf("DocGet error = %v, want rpc error", err)
	}
	if _, err := client.DocSet(ctx, "herdr-app", "mac", 7, value); !IsCode(err, CodeRPC) {
		t.Fatalf("DocSet error = %v, want rpc error", err)
	}

	entries := waitForLog(t, logPath, func(entries []fakeLogEntry) bool {
		get, getOK := findRequest(entries, "doc.get")
		set, setOK := findRequest(entries, "doc.set")
		return getOK && setOK && get.Params != nil && set.Params != nil
	})

	get, _ := findRequest(entries, "doc.get")
	var getParams map[string]json.RawMessage
	if err := json.Unmarshal(get.Params, &getParams); err != nil {
		t.Fatalf("doc.get params: %v", err)
	}
	if string(getParams["name"]) != `"herdr-app"` {
		t.Fatalf("doc.get name = %s", getParams["name"])
	}
	if _, present := getParams["on"]; present {
		t.Fatalf("doc.get sent an empty on: %s", get.Params)
	}

	set, _ := findRequest(entries, "doc.set")
	var setParams struct {
		Name    string          `json:"name"`
		On      string          `json:"on"`
		Version uint64          `json:"version"`
		Value   json.RawMessage `json:"value"`
	}
	if err := json.Unmarshal(set.Params, &setParams); err != nil {
		t.Fatalf("doc.set params: %v", err)
	}
	if setParams.Name != "herdr-app" || setParams.On != "mac" || setParams.Version != 7 {
		t.Fatalf("doc.set params = %+v", setParams)
	}
	if string(setParams.Value) != string(value) {
		t.Fatalf("doc.set value = %s, want %s", setParams.Value, value)
	}
}

func TestDocSetRejectsInvalidAndOversizedValuesLocally(t *testing.T) {
	client, logPath := startFake(t, "")
	defer client.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	oversized := json.RawMessage(strings.Repeat("x", MaxDocValueBytes+1))
	cases := []struct {
		name string
		call func() error
		code ErrorCode
	}{
		{
			name: "oversized value",
			call: func() error {
				_, err := client.DocSet(ctx, "herdr-app", "", 0, oversized)
				return err
			},
			code: CodeRequestTooLarge,
		},
		{
			name: "invalid json",
			call: func() error {
				_, err := client.DocSet(ctx, "herdr-app", "", 0, json.RawMessage(`{"spaces":`))
				return err
			},
			code: CodeProtocol,
		},
		{
			name: "missing value",
			call: func() error {
				_, err := client.DocSet(ctx, "herdr-app", "", 0, nil)
				return err
			},
			code: CodeProtocol,
		},
		{
			name: "missing name",
			call: func() error {
				_, err := client.DocSet(ctx, "", "", 0, json.RawMessage(`{"spaces":[]}`))
				return err
			},
			code: CodeProtocol,
		},
		{
			name: "long name",
			call: func() error {
				_, err := client.DocSet(ctx, strings.Repeat("n", MaxDocNameLength+1), "", 0, json.RawMessage(`{"spaces":[]}`))
				return err
			},
			code: CodeProtocol,
		},
		{
			name: "DocGet missing name",
			call: func() error {
				_, _, err := client.DocGet(ctx, "", "")
				return err
			},
			code: CodeProtocol,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if err := tc.call(); !IsCode(err, tc.code) {
				t.Fatalf("error = %v, want code %s", err, tc.code)
			}
		})
	}

	if _, ok := findRequest(readFakeLog(logPath), "doc.set"); ok {
		t.Fatalf("a rejected doc.set was sent to the core")
	}
}
