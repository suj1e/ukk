package remotescreen

import (
	"bytes"
	"errors"
	"testing"
)

func nal(typ byte, payload ...byte) []byte {
	out := make([]byte, 0, 1+len(payload))
	out = append(out, typ&0x1f)
	out = append(out, payload...)
	return out
}

func sc3(nalu []byte) []byte {
	return append([]byte{0x00, 0x00, 0x01}, nalu...)
}

func sc4(nalu []byte) []byte {
	return append([]byte{0x00, 0x00, 0x00, 0x01}, nalu...)
}

func join(parts ...[]byte) []byte {
	var out []byte
	for _, p := range parts {
		out = append(out, p...)
	}
	return out
}

func TestAnnexBParserGroupsAccessUnits(t *testing.T) {
	sps := nal(nalTypeSPS, 0x42, 0x00, 0x1f)
	pps := nal(nalTypePPS, 0xce, 0x00)
	idr := nal(nalTypeIDRSlice, 0x80)
	slice := nal(nalTypeNonIDRSlice, 0x80, 0xbb)
	next := nal(nalTypeNonIDRSlice, 0x80, 0xcc)

	p := NewAnnexBParser()
	aus, err := p.Feed(join(sc4(sps), sc3(pps), sc4(idr), sc3(slice), sc4(next)))
	if err != nil {
		t.Fatal(err)
	}
	if len(aus) != 1 {
		t.Fatalf("Feed units = %d, want 1", len(aus))
	}
	au := aus[0]
	if !au.SPS || !au.PPS || !au.IDR || !au.Keyframe {
		t.Fatalf("flags SPS=%v PPS=%v IDR=%v key=%v", au.SPS, au.PPS, au.IDR, au.Keyframe)
	}
	if len(au.NALUs) != 3 {
		t.Fatalf("NALUs = %d, want 3", len(au.NALUs))
	}
	if !bytes.Equal(au.NALUs[0], sps) || !bytes.Equal(au.NALUs[1], pps) || !bytes.Equal(au.NALUs[2], idr) {
		t.Fatalf("unexpected NALUs: %x", au.NALUs)
	}
	annexB := au.AnnexB()
	if !bytes.HasPrefix(annexB, []byte{0, 0, 0, 1}) {
		t.Fatalf("AnnexB does not start with four-byte code: %x", annexB[:min(4, len(annexB))])
	}
	if !bytes.Contains(annexB, append([]byte{0, 0, 0, 1}, sps...)) {
		t.Fatalf("AnnexB missing SPS: %x", annexB)
	}

	rest := p.Flush()
	if len(rest) != 1 {
		t.Fatalf("Flush units = %d, want 1", len(rest))
	}
	if rest[0].Keyframe || len(rest[0].NALUs) != 1 {
		t.Fatalf("unexpected trailing unit: %+v", rest[0])
	}
	if len(p.Flush()) != 0 {
		t.Fatal("second Flush should be empty")
	}
}

func TestAnnexBParserFragmentedInput(t *testing.T) {
	sps := nal(nalTypeSPS, 0x42)
	pps := nal(nalTypePPS, 0xce)
	idr := nal(nalTypeIDRSlice, 0x80)
	slice := nal(nalTypeNonIDRSlice, 0x80, 0x01)
	next := nal(nalTypeNonIDRSlice, 0x80, 0x02)
	stream := join(sc4(sps), sc3(pps), sc4(idr), sc3(slice), sc4(next))

	p := NewAnnexBParser()
	var aus []AccessUnit
	for _, b := range stream {
		units, err := p.Feed([]byte{b})
		if err != nil {
			t.Fatalf("Feed at byte: %v", err)
		}
		aus = append(aus, units...)
	}
	aus = append(aus, p.Flush()...)
	if len(aus) != 2 {
		t.Fatalf("units = %d, want 2", len(aus))
	}
	if !aus[0].IDR || len(aus[0].NALUs) != 3 {
		t.Fatalf("first unit = %+v", aus[0])
	}
	if aus[1].IDR || len(aus[1].NALUs) != 1 {
		t.Fatalf("second unit = %+v", aus[1])
	}
}

func TestAnnexBParserThreeAndFourByteStartCodes(t *testing.T) {
	sps := nal(nalTypeSPS, 0x11)
	sei := nal(nalTypeSEI, 0x22, 0x00, 0x03, 0x33)
	slice := nal(nalTypeNonIDRSlice, 0x80)
	next := nal(nalTypeNonIDRSlice, 0x80)
	stream := join(sc4(sps), sc3(sei), sc3(slice), sc4(next), sc4(nal(nalTypeNonIDRSlice, 0x80)))

	p := NewAnnexBParser()
	aus, err := p.Feed(stream)
	if err != nil {
		t.Fatal(err)
	}
	if len(aus) != 1 {
		t.Fatalf("units = %d, want 1", len(aus))
	}
	want := [][]byte{sps, sei, slice}
	if len(aus[0].NALUs) != len(want) {
		t.Fatalf("NALUs = %x", aus[0].NALUs)
	}
	for i := range want {
		if !bytes.Equal(aus[0].NALUs[i], want[i]) {
			t.Fatalf("NALU %d = %x, want %x", i, aus[0].NALUs[i], want[i])
		}
	}
}

func TestAnnexBParserDropsPartialTrailingNALOnFlush(t *testing.T) {
	sps := nal(nalTypeSPS, 0x42)
	pps := nal(nalTypePPS, 0xce)
	idr := nal(nalTypeIDRSlice, 0x80)
	complete := nal(nalTypeNonIDRSlice, 0x80, 0x99)
	partial := []byte{0x00, 0x00, 0x00, 0x01, 0x65, 0xde, 0xad}

	p := NewAnnexBParser()
	aus, err := p.Feed(join(sc4(sps), sc3(pps), sc4(idr), sc3(complete), partial))
	if err != nil {
		t.Fatal(err)
	}
	if len(aus) != 1 || !aus[0].IDR {
		t.Fatalf("Feed units = %+v, want one IDR unit", aus)
	}
	rest := p.Flush()
	if len(rest) != 1 {
		t.Fatalf("Flush units = %d, want 1", len(rest))
	}
	if len(rest[0].NALUs) != 1 || !bytes.Equal(rest[0].NALUs[0], complete) {
		t.Fatalf("partial NAL leaked into flush: %x", rest[0].NALUs)
	}
}

func TestAnnexBParserMultiSlicePicture(t *testing.T) {
	sps := nal(nalTypeSPS, 0x42)
	pps := nal(nalTypePPS, 0xce)
	idr := nal(nalTypeIDRSlice, 0x80)       // first_mb_in_slice = 0
	slice2 := nal(nalTypeNonIDRSlice, 0x40) // first_mb_in_slice = 1
	nextPicture := nal(nalTypeNonIDRSlice, 0x80)
	pending := nal(nalTypeNonIDRSlice, 0x80, 0xff)

	p := NewAnnexBParser()
	aus, err := p.Feed(join(sc4(sps), sc4(pps), sc4(idr), sc3(slice2), sc4(nextPicture), sc3(pending)))
	if err != nil {
		t.Fatal(err)
	}
	if len(aus) != 1 {
		t.Fatalf("units = %d, want 1", len(aus))
	}
	if len(aus[0].NALUs) != 4 {
		t.Fatalf("multi-slice NALUs = %d, want 4", len(aus[0].NALUs))
	}
	if !aus[0].IDR {
		t.Fatal("multi-slice picture lost IDR flag")
	}
	rest := p.Flush()
	if len(rest) != 1 || len(rest[0].NALUs) != 1 || rest[0].IDR {
		t.Fatalf("next picture = %+v", rest)
	}
}

func TestAnnexBParserGarbageAndSplitStartCode(t *testing.T) {
	sps := nal(nalTypeSPS, 0x42)
	slice := nal(nalTypeNonIDRSlice, 0x80)
	next := nal(nalTypeNonIDRSlice, 0x80)

	p := NewAnnexBParser()
	if _, err := p.Feed([]byte("garbage")); err != nil {
		t.Fatal(err)
	}
	if aus, err := p.Feed([]byte{0x00, 0x00}); err != nil || len(aus) != 0 {
		t.Fatalf("split prefix: units=%v err=%v", aus, err)
	}
	// Leading zero completes a four-byte start code; then ordinary 3-byte code.
	stream := join([]byte{0x00}, sc3(sps), sc3(slice), sc4(next), sc4(nal(nalTypeNonIDRSlice, 0x80)))
	aus, err := p.Feed(stream)
	if err != nil {
		t.Fatal(err)
	}
	if len(aus) != 1 || len(aus[0].NALUs) != 2 {
		t.Fatalf("units = %+v", aus)
	}
	if !bytes.Equal(aus[0].NALUs[0], sps) {
		t.Fatalf("garbage was not trimmed: %x", aus[0].NALUs[0])
	}
}

func TestAnnexBParserBound(t *testing.T) {
	p := NewAnnexBParser()
	p.limit = 8
	_, err := p.Feed([]byte("0123456789"))
	if !errors.Is(err, ErrFrameTooLarge) {
		t.Fatalf("err = %v, want ErrFrameTooLarge", err)
	}
}
