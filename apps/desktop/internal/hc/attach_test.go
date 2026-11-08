package hc

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
	"testing"
	"time"
)

func framesOf(entries []fakeLogEntry, channel uint32, kind uint8) []fakeLogEntry {
	var out []fakeLogEntry
	for _, entry := range entries {
		if entry.Tag == "frame" && entry.Channel == channel && entry.Kind == kind {
			out = append(out, entry)
		}
	}
	return out
}

func TestAttachCreditAccounting(t *testing.T) {
	client, logPath := startFake(t, "credit")
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	att, err := client.Attach(ctx, "t1", 80, 24, AttachOptions{})
	if err != nil {
		t.Fatalf("Attach: %v", err)
	}
	defer att.Close()
	if att.Channel() != 3 {
		t.Fatalf("channel = %d", att.Channel())
	}
	if !att.AutomaticInput() || !att.ResizeNotifications() {
		t.Fatalf("capabilities: automatic=%t resize=%t", att.AutomaticInput(), att.ResizeNotifications())
	}

	first := make([]byte, 4)
	if _, err := io.ReadFull(att, first); err != nil {
		t.Fatalf("first read: %v", err)
	}
	if string(first) != "0123" {
		t.Fatalf("first chunk = %q", first)
	}
	waitForLog(t, logPath, func(entries []fakeLogEntry) bool {
		return len(framesOf(entries, 3, KindCredit)) >= 1
	})

	second := make([]byte, 6)
	if _, err := io.ReadFull(att, second); err != nil {
		t.Fatalf("second read: %v", err)
	}
	if string(second) != "456789" {
		t.Fatalf("second chunk = %q", second)
	}

	third := make([]byte, 10)
	if _, err := io.ReadFull(att, third); err != nil {
		t.Fatalf("third read (sent only after credit): %v", err)
	}
	if string(third) != "abcdefghij" {
		t.Fatalf("third chunk = %q", third)
	}

	entries := waitForLog(t, logPath, func(entries []fakeLogEntry) bool {
		return len(framesOf(entries, 3, KindCredit)) >= 2
	})
	credits := framesOf(entries, 3, KindCredit)
	firstCredit := binary.LittleEndian.Uint32(credits[0].Payload)
	secondCredit := binary.LittleEndian.Uint32(credits[1].Payload)
	if firstCredit != 4 || secondCredit != 6 {
		t.Fatalf("credits = %d, %d; want 4, 6", firstCredit, secondCredit)
	}
	if got := att.CreditedBytes(); got != 20 {
		t.Fatalf("CreditedBytes = %d", got)
	}
}

func TestObserveRefusesInputLocally(t *testing.T) {
	client, logPath := startFake(t, "framed")
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	att, err := client.Attach(ctx, "t1", 80, 24, AttachOptions{Observe: true, AutomaticInput: true})
	if err != nil {
		t.Fatalf("Attach: %v", err)
	}
	if _, err := att.Write([]byte("x")); !IsCode(err, CodeObserveReadOnly) {
		t.Fatalf("Write err = %v", err)
	}
	if _, err := att.AutomaticWrite([]byte("x")); !IsCode(err, CodeObserveReadOnly) {
		t.Fatalf("AutomaticWrite err = %v", err)
	}
	if err := att.Resize(100, 30); !IsCode(err, CodeObserveReadOnly) {
		t.Fatalf("Resize err = %v", err)
	}
	if err := att.Focus(true); !IsCode(err, CodeObserveReadOnly) {
		t.Fatalf("Focus err = %v", err)
	}

	time.Sleep(100 * time.Millisecond)
	entries := readFakeLog(logPath)
	for _, kind := range []uint8{KindData, KindAutomaticInput, KindResize, KindFocus} {
		if got := framesOf(entries, 3, kind); len(got) != 0 {
			t.Fatalf("observe stream sent frame kind %d: %+v", kind, got)
		}
	}
	if err := att.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	entries = waitForLog(t, logPath, func(entries []fakeLogEntry) bool {
		return len(framesOf(entries, 3, KindClose)) >= 1
	})
	if len(framesOf(entries, 3, KindClose)) < 1 {
		t.Fatal("close frame was not sent")
	}
}

func TestResizeFocusFactsAndCloseFrames(t *testing.T) {
	client, logPath := startFake(t, "framed")
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	att, err := client.Attach(ctx, "t1", 80, 24, AttachOptions{})
	if err != nil {
		t.Fatalf("Attach: %v", err)
	}
	if err := att.Resize(120, 40); err != nil {
		t.Fatalf("Resize: %v", err)
	}
	if err := att.Focus(true); err != nil {
		t.Fatalf("Focus: %v", err)
	}
	if err := att.SendFacts(Display{FG: new("#ffffff")}); err != nil {
		t.Fatalf("SendFacts: %v", err)
	}
	if err := att.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	entries := waitForLog(t, logPath, func(entries []fakeLogEntry) bool {
		return len(framesOf(entries, 3, KindResize)) >= 1 &&
			len(framesOf(entries, 3, KindFocus)) >= 1 &&
			len(framesOf(entries, 3, KindFacts)) >= 1 &&
			len(framesOf(entries, 3, KindClose)) >= 1
	})
	resize := framesOf(entries, 3, KindResize)[0]
	if len(resize.Payload) != 4 {
		t.Fatalf("resize payload = %v", resize.Payload)
	}
	if cols := binary.LittleEndian.Uint16(resize.Payload[0:2]); cols != 120 {
		t.Fatalf("resize cols = %d", cols)
	}
	if rows := binary.LittleEndian.Uint16(resize.Payload[2:4]); rows != 40 {
		t.Fatalf("resize rows = %d", rows)
	}
	focus := framesOf(entries, 3, KindFocus)[0]
	if len(focus.Payload) != 1 || focus.Payload[0] != 1 {
		t.Fatalf("focus payload = %v", focus.Payload)
	}
	facts := framesOf(entries, 3, KindFacts)[0]
	var display Display
	if err := json.Unmarshal(facts.Payload, &display); err != nil {
		t.Fatalf("facts payload: %v", err)
	}
	if display.FG == nil || *display.FG != "#ffffff" {
		t.Fatalf("facts = %+v", display)
	}
	if closeFrame := framesOf(entries, 3, KindClose)[0]; len(closeFrame.Payload) != 0 {
		t.Fatalf("client CLOSE payload = %v, want empty", closeFrame.Payload)
	}
}

func TestCoreCloseExposesTypedReason(t *testing.T) {
	client, _ := startFake(t, "close")
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	att, err := client.Attach(ctx, "t1", 80, 24, AttachOptions{})
	if err != nil {
		t.Fatalf("Attach: %v", err)
	}
	buf := make([]byte, 8)
	_, err = att.Read(buf)
	var closed *StreamClosedError
	if !errors.As(err, &closed) {
		t.Fatalf("Read err = %v, want StreamClosedError", err)
	}
	if closed.Close.Reason != "exited" || closed.Close.ExitCode == nil || *closed.Close.ExitCode != 7 {
		t.Fatalf("close info = %+v", closed.Close)
	}
	if closed.Close.Signal != nil {
		t.Fatalf("signal = %v, want nil", *closed.Close.Signal)
	}
	info, ok := att.CloseInfo()
	if !ok || info.Reason != "exited" || info.FinishedMS == nil || *info.FinishedMS != 123 {
		t.Fatalf("CloseInfo = %+v, %t", info, ok)
	}
	if err := att.Close(); err != nil {
		t.Fatalf("Close after core close: %v", err)
	}
}

func TestCoreResizeAndFacts(t *testing.T) {
	client, _ := startFake(t, "core-resize")
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	att, err := client.Attach(ctx, "t1", 80, 24, AttachOptions{Observe: true, ResizeNotifications: true})
	if err != nil {
		t.Fatalf("Attach: %v", err)
	}
	defer att.Close()
	deadline := time.Now().Add(2 * time.Second)
	for {
		cols, rows := att.Size()
		facts := att.Facts()
		if cols == 120 && rows == 40 && len(facts) > 0 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("size = %d,%d facts=%s", cols, rows, facts)
		}
		time.Sleep(10 * time.Millisecond)
	}
	var display Display
	if err := json.Unmarshal(att.Facts(), &display); err != nil {
		t.Fatalf("facts: %v", err)
	}
	if display.FG == nil || *display.FG != "#ffffff" {
		t.Fatalf("facts = %+v", display)
	}
}

func TestUnknownFrameKindsAreSkipped(t *testing.T) {
	client, _ := startFake(t, "unknown")
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	att, err := client.Attach(ctx, "t1", 80, 24, AttachOptions{})
	if err != nil {
		t.Fatalf("Attach: %v", err)
	}
	defer att.Close()
	buf := make([]byte, 2)
	if _, err := io.ReadFull(att, buf); err != nil {
		t.Fatalf("Read after unknown kind: %v", err)
	}
	if string(buf) != "ok" {
		t.Fatalf("payload = %q", buf)
	}
}
