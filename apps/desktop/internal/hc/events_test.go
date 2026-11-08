package hc

import (
	"context"
	"errors"
	"testing"
	"time"
)

func TestEventSequenceGapAndMissedSnapshot(t *testing.T) {
	client, _ := startFake(t, "events")
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	sub, err := client.SubscribeEvents(ctx, SubscribeOptions{})
	if err != nil {
		t.Fatalf("SubscribeEvents: %v", err)
	}
	defer sub.Close()

	if got := sub.InitialSnapshot(); len(got) != 1 || got[0].Address != "t10" {
		t.Fatalf("initial snapshot = %+v", got)
	}

	first := nextEvent(t, sub, 2*time.Second)
	if first.Seq != 12 || first.Type != "title" || first.Terminal != "t10" {
		t.Fatalf("first event = %+v", first)
	}

	var gap *GapError
	deadline := time.Now().Add(2 * time.Second)
	for gap == nil {
		select {
		case err := <-sub.Errors():
			var candidate *GapError
			if errors.As(err, &candidate) && !candidate.Recovered {
				gap = candidate
			}
		case <-time.After(time.Until(deadline)):
			t.Fatal("sequence gap was not reported")
		}
	}
	if gap.Expected != 11 || gap.Got != 12 {
		t.Fatalf("gap = %+v", gap)
	}

	recovered := false
	snapshotDeadline := time.Now().Add(2 * time.Second)
	for !recovered || len(sub.Snapshot()) != 1 || sub.Snapshot()[0].Address != "t20" {
		select {
		case err := <-sub.Errors():
			var candidate *GapError
			if errors.As(err, &candidate) && candidate.Recovered {
				recovered = true
			}
		case <-time.After(10 * time.Millisecond):
		}
		if time.Now().After(snapshotDeadline) {
			t.Fatalf("missed snapshot not applied; snapshot=%+v recovered=%t", sub.Snapshot(), recovered)
		}
	}
	if sub.MissedCount() != 1 {
		t.Fatalf("MissedCount = %d", sub.MissedCount())
	}

	// seq 19 is below the missed snapshot and must be ignored; seq 21 follows.
	last := nextEvent(t, sub, 2*time.Second)
	if last.Seq != 21 || last.Terminal != "t11" {
		t.Fatalf("event after missed = %+v", last)
	}
	select {
	case extra := <-sub.Events():
		t.Fatalf("unexpected stale event %+v", extra)
	case <-time.After(100 * time.Millisecond):
	}
}

func TestFilteredSubscriptionDoesNotReportGaps(t *testing.T) {
	client, _ := startFake(t, "events")
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	sub, err := client.SubscribeEvents(ctx, SubscribeOptions{Terminal: "t1"})
	if err != nil {
		t.Fatalf("SubscribeEvents: %v", err)
	}
	defer sub.Close()
	first := nextEvent(t, sub, 2*time.Second)
	if first.Seq != 12 {
		t.Fatalf("first filtered event = %+v", first)
	}
	select {
	case err := <-sub.Errors():
		var gap *GapError
		if errors.As(err, &gap) && !gap.Recovered {
			t.Fatalf("filtered subscription reported a gap: %v", err)
		}
	case <-time.After(150 * time.Millisecond):
	}
}

func nextEvent(t *testing.T, sub *Subscription, limit time.Duration) Event {
	t.Helper()
	select {
	case event, ok := <-sub.Events():
		if !ok {
			t.Fatal("event channel closed")
		}
		return event
	case <-time.After(limit):
		t.Fatal("timed out waiting for event")
		return Event{}
	}
}
