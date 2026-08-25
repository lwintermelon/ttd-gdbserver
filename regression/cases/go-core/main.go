// One realistic Go program covering the Delve-side surface: goroutines and a
// shared atomic (watchpoint), deep recursion with 2 KiB frames (stack growth
// and locals after the move), and an unhandled nil-pointer panic at the end
// (the recorded process exit).
//
// Build: go build -gcflags="all=-N -l" -o core.exe .
package main

import (
	"fmt"
	"sync"
	"sync/atomic"
)

const workers = 3
const perWorker = 4

//go:noinline
func consume(id int, in <-chan int, hits *atomic.Int64, wg *sync.WaitGroup) {
	defer wg.Done()
	for v := range in {
		hits.Add(int64(v))
	}
}

//go:noinline
func deep(n int) int {
	var pad [2048]byte
	pad[n%len(pad)] = byte(n)
	if n == 0 {
		return int(pad[0])
	}
	return deep(n-1) + int(pad[1])
}

type node struct {
	next *node
	val  int
}

//go:noinline
func crash(n *node) int {
	return n.next.val
}

func main() {
	var wg sync.WaitGroup
	var hits atomic.Int64
	chans := make([]chan int, workers)

	for i := 0; i < workers; i++ {
		chans[i] = make(chan int, perWorker)
		wg.Add(1)
		go consume(i, chans[i], &hits, &wg)
	}
	for i := 0; i < workers; i++ {
		for j := 0; j < perWorker; j++ {
			chans[i] <- j * 3
		}
		close(chans[i])
	}
	wg.Wait()

	fmt.Println("hits:", hits.Load())
	fmt.Println("deep:", deep(3000))

	head := &node{next: &node{val: 7}}
	fmt.Println("crash:", crash(head.next))
}
