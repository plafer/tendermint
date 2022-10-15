package tx

import (
	"sync"
	"sync/atomic"

	"github.com/tendermint/tendermint/types"
)

// mempoolTx is a transaction that successfully ran
type MempoolTx struct {
	Height    int64    // height that this tx had been validated in
	GasWanted int64    // amount of gas this tx states it will require
	Tx        types.Tx //

	// ids of peers who've sent us this tx (as a map for quick lookups).
	// senders: PeerID -> bool
	Senders sync.Map
}

// Height returns the height for this transaction
func (memTx *MempoolTx) GetHeightAtomic() int64 {
	return atomic.LoadInt64(&memTx.Height)
}
