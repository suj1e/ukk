package corelayout

import (
	"bytes"
	"encoding/json"
	"fmt"
)

// object collects the known fields of a JSON object before marshaling. It
// keeps the encoding deterministic through encoding/json's sorted map keys.
type object map[string]any

func (o object) addString(key string, v *string) {
	if v != nil {
		o[key] = *v
	}
}

func (o object) addFloat(key string, v *float32) {
	if v != nil {
		o[key] = *v
	}
}

func (o object) addBool(key string, v bool) {
	if v {
		o[key] = v
	}
}

func (o object) addRaw(key string, v json.RawMessage) {
	if len(v) > 0 && !isJSONNull(v) {
		o[key] = v
	}
}

func (o object) marshal(extra map[string]json.RawMessage) ([]byte, error) {
	out := make(map[string]json.RawMessage, len(o)+len(extra))
	for key, value := range o {
		body, err := json.Marshal(value)
		if err != nil {
			return nil, err
		}
		out[key] = body
	}
	for key, value := range extra {
		if _, known := out[key]; known || len(value) == 0 {
			continue
		}
		out[key] = value
	}
	return json.Marshal(out)
}

// decodeObject decodes a JSON object while leaving every value raw. JSON null
// decodes to an empty object so a malformed nested value fails validation
// instead of the decoder inventing fields.
func decodeObject(data []byte) (map[string]json.RawMessage, error) {
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil, err
	}
	if raw == nil {
		raw = map[string]json.RawMessage{}
	}
	return raw, nil
}

// takeValue decodes raw[key] into dst and removes the key. A missing key leaves
// dst untouched.
func takeValue(raw map[string]json.RawMessage, key string, dst any) error {
	body, ok := raw[key]
	if !ok {
		return nil
	}
	delete(raw, key)
	if err := json.Unmarshal(body, dst); err != nil {
		return fmt.Errorf("field %q: %w", key, err)
	}
	return nil
}

// extras returns the fields that were not consumed, or nil when there are
// none.
func extras(raw map[string]json.RawMessage) map[string]json.RawMessage {
	if len(raw) == 0 {
		return nil
	}
	return raw
}

func isJSONNull(raw json.RawMessage) bool {
	return bytes.Equal(bytes.TrimSpace(raw), []byte("null"))
}
