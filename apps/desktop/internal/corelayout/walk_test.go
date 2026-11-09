package corelayout

import "testing"

func TestNodeWalkHelpers(t *testing.T) {
	one := "t1"
	two := "t2"
	root := Node{
		ID:   "root",
		Type: TypeRow,
		Children: []Node{
			{ID: "a", Type: TypeBlock, Content: &Content{Terminal: &one}},
			{ID: "nested", Type: TypeColumn, Children: []Node{
				{ID: "b", Type: TypeBlock, Content: &Content{Terminal: &two}},
				{ID: "files", Type: TypeBlock, Content: &Content{Mod: new("files")}},
			}},
		},
	}
	if root.Find("nested") == nil || root.Find("nested").ID != "nested" {
		t.Fatal("Find missed a container")
	}
	if root.Find("missing") != nil {
		t.Fatal("Find invented a node")
	}
	blocks := root.Blocks()
	if len(blocks) != 3 {
		t.Fatalf("blocks = %d, want 3", len(blocks))
	}
	addresses := root.TerminalAddresses()
	if len(addresses) != 2 || addresses[0] != "t1" || addresses[1] != "t2" {
		t.Fatalf("addresses = %v", addresses)
	}
	if !root.ContainsPane("files") {
		t.Fatal("ContainsPane missed a block")
	}
	if root.ContainsPane("missing") {
		t.Fatal("ContainsPane invented a block")
	}
}
