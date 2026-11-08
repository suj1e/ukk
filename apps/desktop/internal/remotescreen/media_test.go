package remotescreen

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"
)

func TestPublisherICEServers(t *testing.T) {
	servers, err := publisherICEServers(PublisherOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if len(servers) != 1 || len(servers[0].URLs) != 1 || servers[0].URLs[0] != DefaultSTUNURL {
		t.Fatalf("default servers = %+v", servers)
	}

	servers, err = publisherICEServers(PublisherOptions{STUNURLs: []string{}})
	if err != nil {
		t.Fatal(err)
	}
	if len(servers) != 0 {
		t.Fatalf("empty STUN list = %+v", servers)
	}

	servers, err = publisherICEServers(PublisherOptions{
		STUNURLs: []string{"stun:stun.example.test:3478"},
		TURN: &TURNOptions{
			URLs:       []string{"turn:turn.example.test:3478?transport=udp"},
			Username:   "user",
			Credential: "secret",
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(servers) != 2 || servers[1].Username != "user" || servers[1].Credential != "secret" {
		t.Fatalf("servers = %+v", servers)
	}
}

func TestPublisherOptionsValidation(t *testing.T) {
	if _, err := publisherICEServers(PublisherOptions{STUNURLs: []string{"http://nope"}}); !errors.Is(err, ErrInvalidPublisherOptions) {
		t.Fatalf("bad STUN = %v", err)
	}
	if _, err := publisherICEServers(PublisherOptions{TURN: &TURNOptions{URLs: []string{"turn:host"}}}); !errors.Is(err, ErrInvalidPublisherOptions) {
		t.Fatalf("TURN without credentials = %v", err)
	}
	if _, err := NewPublisher(PublisherOptions{STUNURLs: []string{}, MTU: 100}); !errors.Is(err, ErrInvalidPublisherOptions) {
		t.Fatalf("bad MTU = %v", err)
	}
}

func TestRTPTimestamp(t *testing.T) {
	tests := []struct {
		pts  time.Duration
		want uint32
	}{
		{0, 0},
		{time.Second, 90000},
		{time.Second / 2, 45000},
		{time.Second / 90000, 1},
		{time.Duration(0), 0},
	}
	for _, tc := range tests {
		if got := rtpTimestamp(tc.pts, 90000); got != tc.want {
			t.Fatalf("rtpTimestamp(%v) = %d, want %d", tc.pts, got, tc.want)
		}
	}
}

func TestAnnexBContainsIDR(t *testing.T) {
	idr := nal(nalTypeIDRSlice, 0x80)
	slice := nal(nalTypeNonIDRSlice, 0x80)
	if !annexBContainsIDR(sc4(idr)) {
		t.Fatal("four-byte IDR not detected")
	}
	if !annexBContainsIDR(sc3(idr)) {
		t.Fatal("three-byte IDR not detected")
	}
	if !annexBContainsIDR(join(sc4(nal(nalTypeSPS, 0x42)), sc3(idr))) {
		t.Fatal("IDR after SPS not detected")
	}
	if annexBContainsIDR(join(sc4(nal(nalTypeSPS, 0x42)), sc3(slice))) {
		t.Fatal("non-IDR reported as keyframe")
	}
	if !annexBContainsIDR(idr) {
		t.Fatal("raw IDR not detected")
	}
}

func TestPublisherRequiresOfferAndDetectsKeyframe(t *testing.T) {
	pub, err := NewPublisher(PublisherOptions{STUNURLs: []string{}})
	if err != nil {
		t.Fatal(err)
	}
	defer pub.Close()

	frame := Frame{Data: sc4(nal(nalTypeIDRSlice, 0x80)), PTS: 0}
	if err := pub.WriteFrame(frame); !errors.Is(err, ErrPublisherNotReady) {
		t.Fatalf("WriteFrame before offer = %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if _, err := pub.CreateOffer(ctx); err != nil {
		t.Fatal(err)
	}

	if err := pub.WriteFrame(frame); err != nil {
		t.Fatalf("WriteFrame after offer: %v", err)
	}
	if pts, ok := pub.LastKeyframePTS(); !ok || pts != 0 {
		t.Fatalf("keyframe PTS = %v, %v", pts, ok)
	}
	if err := pub.WriteFrame(Frame{Data: []byte{}, PTS: time.Second}); !errors.Is(err, ErrInvalidFrame) {
		t.Fatalf("empty frame = %v", err)
	}
	if err := pub.WriteFrame(Frame{Data: frame.Data, PTS: -time.Second}); !errors.Is(err, ErrInvalidFrame) {
		t.Fatalf("negative PTS = %v", err)
	}
}

func TestPublisherOfferAnswerLoopback(t *testing.T) {
	pub, err := NewPublisher(PublisherOptions{STUNURLs: []string{}})
	if err != nil {
		t.Fatal(err)
	}
	defer pub.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	offer, err := pub.CreateOffer(ctx)
	if err != nil {
		t.Fatalf("CreateOffer: %v", err)
	}
	if offer.Type != webrtc.SDPTypeOffer {
		t.Fatalf("offer type = %s", offer.Type)
	}
	if !strings.Contains(offer.SDP, "m=video") {
		t.Fatalf("offer has no video section:\n%s", offer.SDP)
	}
	if !strings.Contains(offer.SDP, "H264/90000") {
		t.Fatalf("offer has no H264 codec:\n%s", offer.SDP)
	}

	answerer, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatal(err)
	}
	defer answerer.Close()

	if err := answerer.SetRemoteDescription(offer); err != nil {
		t.Fatalf("answerer SetRemoteDescription: %v", err)
	}
	gather := webrtc.GatheringCompletePromise(answerer)
	answer, err := answerer.CreateAnswer(nil)
	if err != nil {
		t.Fatalf("CreateAnswer: %v", err)
	}
	if err := answerer.SetLocalDescription(answer); err != nil {
		t.Fatalf("answerer SetLocalDescription: %v", err)
	}
	select {
	case <-gather:
	case <-ctx.Done():
		t.Fatalf("answerer ICE gathering: %v", ctx.Err())
	}
	localAnswer := answerer.LocalDescription()
	if localAnswer == nil {
		t.Fatal("answerer has no local description")
	}
	if err := pub.AcceptAnswer(*localAnswer); err != nil {
		t.Fatalf("AcceptAnswer: %v", err)
	}

	deadline := time.Now().Add(8 * time.Second)
	for {
		state := pub.ConnectionState()
		switch state {
		case webrtc.PeerConnectionStateConnected:
			return
		case webrtc.PeerConnectionStateFailed, webrtc.PeerConnectionStateClosed:
			t.Fatalf("loopback connection state = %s", state)
		}
		if time.Now().After(deadline) {
			t.Fatalf("loopback connection did not establish; state = %s", state)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func TestPublisherClosed(t *testing.T) {
	pub, err := NewPublisher(PublisherOptions{STUNURLs: []string{}})
	if err != nil {
		t.Fatal(err)
	}
	if err := pub.Close(); err != nil {
		t.Fatal(err)
	}
	if err := pub.Close(); err != nil {
		t.Fatalf("second Close = %v", err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	if _, err := pub.CreateOffer(ctx); !errors.Is(err, ErrPublisherClosed) {
		t.Fatalf("CreateOffer after close = %v", err)
	}
	if err := pub.WriteFrame(Frame{Data: sc4(nal(nalTypeIDRSlice, 0x80))}); !errors.Is(err, ErrPublisherClosed) {
		t.Fatalf("WriteFrame after close = %v", err)
	}
}

func TestPublisherCloseStopsAttachedCapture(t *testing.T) {
	pub, err := NewPublisher(PublisherOptions{STUNURLs: []string{}})
	if err != nil {
		t.Fatal(err)
	}
	capture := newStreamCapture()
	if err := pub.AttachCapture(capture); err != nil {
		t.Fatal(err)
	}
	if err := pub.AttachCapture(capture); err == nil {
		t.Fatal("second capture attachment accepted")
	}
	if err := pub.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if _, stopped := capture.status(); !stopped {
		t.Fatal("publisher close did not stop attached capture")
	}
}
