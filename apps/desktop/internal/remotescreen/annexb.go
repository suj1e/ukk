package remotescreen

import (
	"bytes"
	"fmt"
)

// Annex-B NAL unit types used by the H.264 baseline stream produced by
// ffmpeg.
const (
	nalTypeNonIDRSlice = 1
	nalTypePartitionA  = 2
	nalTypePartitionB  = 3
	nalTypePartitionC  = 4
	nalTypeIDRSlice    = 5
	nalTypeSEI         = 6
	nalTypeSPS         = 7
	nalTypePPS         = 8
	nalTypeAUD         = 9
)

const (
	// defaultAnnexBLimit bounds bytes buffered while waiting for the next
	// start code. ffmpeg can emit large IDR frames; this is a safety bound on
	// an untrusted encoder, not a quality setting.
	defaultAnnexBLimit = 16 << 20
	startCode3         = "\x00\x00\x01"
	startCode4         = "\x00\x00\x00\x01"
)

// AccessUnit is one coded picture with its parameter sets and other
// non-VCL NAL units. NALUs do not include start codes.
type AccessUnit struct {
	NALUs    [][]byte
	SPS      bool
	PPS      bool
	IDR      bool
	Keyframe bool
}

// AnnexB renders the access unit as a byte stream with four-byte start codes.
func (a AccessUnit) AnnexB() []byte {
	var out bytes.Buffer
	for _, nalu := range a.NALUs {
		out.WriteString(startCode4)
		out.Write(nalu)
	}
	return out.Bytes()
}

// AnnexBParser incrementally splits an Annex-B byte stream into access units.
//
// The parser accepts arbitrary chunk boundaries, including a start code split
// across two chunks. A NAL unit is only emitted after the following start code
// proves where it ends, so the last partially received NAL stays buffered.
// Flush drops that partial trailing NAL and emits any already-complete access
// unit.
type AnnexBParser struct {
	buf     []byte
	current [][]byte // NALU payloads, start codes stripped
	hasVCL  bool
	sps     bool
	pps     bool
	idr     bool
	limit   int
}

// NewAnnexBParser returns a parser with the default input bound.
func NewAnnexBParser() *AnnexBParser {
	return &AnnexBParser{limit: defaultAnnexBLimit}
}

// Feed appends chunk and returns every access unit completed by this chunk.
func (p *AnnexBParser) Feed(chunk []byte) ([]AccessUnit, error) {
	if p.limit <= 0 {
		p.limit = defaultAnnexBLimit
	}
	if len(p.buf)+len(chunk) > p.limit {
		return nil, fmt.Errorf("%w: %d bytes pending", ErrFrameTooLarge, len(p.buf)+len(chunk))
	}
	p.buf = append(p.buf, chunk...)

	var out []AccessUnit

	// Discard bytes before the first start code. Keep a short tail because a
	// start code may be split across chunks.
	scStart, naluStart, ok := findStartCode(p.buf, 0)
	if !ok {
		if len(p.buf) > len(startCode4)-1 {
			p.buf = append(p.buf[:0], p.buf[len(p.buf)-(len(startCode4)-1):]...)
		}
		return nil, nil
	}
	if scStart > 0 {
		p.buf = p.buf[scStart:]
		naluStart -= scStart
	}

	for {
		nextSC, nextNALU, found := findStartCode(p.buf, naluStart)
		if !found {
			break
		}
		nalu := append([]byte(nil), p.buf[naluStart:nextSC]...)
		p.addNAL(nalu, &out)

		p.buf = p.buf[nextSC:]
		naluStart = nextNALU - nextSC
	}

	return out, nil
}

// Flush emits the access unit assembled so far and drops any trailing NAL
// whose end has not been observed. The parser is reset and can be reused.
func (p *AnnexBParser) Flush() []AccessUnit {
	var out []AccessUnit
	if au, ok := p.takeAccessUnit(); ok {
		out = append(out, au)
	}
	p.Reset()
	return out
}

// Reset discards all buffered input and parser state.
func (p *AnnexBParser) Reset() {
	p.buf = nil
	p.current = nil
	p.hasVCL = false
	p.sps = false
	p.pps = false
	p.idr = false
}

func (p *AnnexBParser) addNAL(nalu []byte, out *[]AccessUnit) {
	if len(nalu) == 0 {
		return
	}
	typ := nalu[0] & 0x1f

	if typ == nalTypeAUD {
		if au, ok := p.takeAccessUnit(); ok {
			*out = append(*out, au)
		}
		p.current = append(p.current, nalu)
		return
	}

	if isVCLType(typ) {
		if p.hasVCL && sliceStartsPicture(nalu) {
			if au, ok := p.takeAccessUnit(); ok {
				*out = append(*out, au)
			}
		}
		p.hasVCL = true
	}

	switch typ {
	case nalTypeSPS:
		p.sps = true
	case nalTypePPS:
		p.pps = true
	case nalTypeIDRSlice:
		p.idr = true
	}
	p.current = append(p.current, nalu)
}

func (p *AnnexBParser) takeAccessUnit() (AccessUnit, bool) {
	if len(p.current) == 0 {
		return AccessUnit{}, false
	}
	nalus := make([][]byte, len(p.current))
	copy(nalus, p.current)
	au := AccessUnit{
		NALUs:    nalus,
		SPS:      p.sps,
		PPS:      p.pps,
		IDR:      p.idr,
		Keyframe: p.idr,
	}
	p.current = nil
	p.hasVCL = false
	p.sps = false
	p.pps = false
	p.idr = false
	return au, true
}

// findStartCode locates the next Annex-B start code at or after from. It
// returns the index where the start code begins (including the optional
// leading zero of a four-byte code) and the index of the first NAL byte.
func findStartCode(buf []byte, from int) (scStart, naluStart int, ok bool) {
	if from < 0 {
		from = 0
	}
	for i := from + 2; i < len(buf); i++ {
		if buf[i] != 0x01 || buf[i-1] != 0x00 || buf[i-2] != 0x00 {
			continue
		}
		scStart = i - 2
		if scStart > 0 && buf[scStart-1] == 0x00 {
			scStart--
		}
		return scStart, i + 1, true
	}
	return 0, 0, false
}

func isVCLType(typ byte) bool {
	return typ >= nalTypeNonIDRSlice && typ <= nalTypeIDRSlice
}

// sliceStartsPicture reports whether a VCL NAL starts a new coded picture,
// based on first_mb_in_slice. Multi-slice pictures keep their slices in one
// access unit. Malformed slice headers are treated as picture starts so an
// encoder error cannot merge unrelated pictures.
func sliceStartsPicture(nalu []byte) bool {
	if len(nalu) < 2 {
		return true
	}
	rbsp := removeEmulationPrevention(nalu[1:])
	br := bitReader{data: rbsp}
	firstMB, ok := br.ue()
	if !ok {
		return true
	}
	return firstMB == 0
}

// removeEmulationPrevention strips 0x03 from 00 00 03 sequences in an RBSP
// payload so Exp-Golomb fields can be read.
func removeEmulationPrevention(in []byte) []byte {
	out := make([]byte, 0, len(in))
	zeros := 0
	for _, b := range in {
		if zeros >= 2 && b == 0x03 {
			zeros = 0
			continue
		}
		out = append(out, b)
		if b == 0x00 {
			zeros++
		} else {
			zeros = 0
		}
	}
	return out
}

type bitReader struct {
	data []byte
	pos  int
}

func (r *bitReader) bit() (uint32, bool) {
	if r.pos >= len(r.data)*8 {
		return 0, false
	}
	b := (r.data[r.pos/8] >> uint(7-r.pos%8)) & 1
	r.pos++
	return uint32(b), true
}

func (r *bitReader) ue() (uint32, bool) {
	leadingZeros := 0
	for {
		b, ok := r.bit()
		if !ok {
			return 0, false
		}
		if b == 1 {
			break
		}
		leadingZeros++
		if leadingZeros > 31 {
			return 0, false
		}
	}
	var value uint32
	for i := 0; i < leadingZeros; i++ {
		b, ok := r.bit()
		if !ok {
			return 0, false
		}
		value = value<<1 | b
	}
	return (1 << uint(leadingZeros)) - 1 + value, true
}
