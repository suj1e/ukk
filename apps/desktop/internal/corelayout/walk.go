package corelayout

// Find returns the node with id, or nil.
func (n *Node) Find(id string) *Node {
	if n == nil {
		return nil
	}
	if n.ID == id {
		return n
	}
	for i := range n.Children {
		if found := n.Children[i].Find(id); found != nil {
			return found
		}
	}
	return nil
}

// Blocks returns every block node in the tree, depth first.
func (n *Node) Blocks() []*Node {
	var out []*Node
	var walk func(*Node)
	walk = func(node *Node) {
		if node == nil {
			return
		}
		if node.Type == TypeBlock {
			out = append(out, node)
		}
		for i := range node.Children {
			walk(&node.Children[i])
		}
	}
	walk(n)
	return out
}

// TerminalAddresses returns the terminal addresses of every terminal block,
// in tree order.
func (n *Node) TerminalAddresses() []string {
	var out []string
	for _, block := range n.Blocks() {
		if block.Content != nil && block.Content.Terminal != nil && *block.Content.Terminal != "" {
			out = append(out, *block.Content.Terminal)
		}
	}
	return out
}

// ContainsPane reports whether the tree has a block with id.
func (n *Node) ContainsPane(id string) bool {
	return n.Find(id) != nil
}
