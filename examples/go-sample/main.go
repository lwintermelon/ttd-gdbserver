package main

import "fmt"

type Point struct {
	X, Y int
	Name string
}

// A plain function to step into and inspect locals.
func add(a, b int) int {
	sum := a + b
	return sum
}

// A function to set breakpoints on and reverse-step over.
func compute(points []Point) int {
	total := 0
	for _, p := range points {
		total += add(p.X, p.Y)
	}
	return total
}

func main() {
	points := []Point{
		{X: 1, Y: 2, Name: "alpha"},
		{X: 3, Y: 4, Name: "beta"},
		{X: 5, Y: 6, Name: "gamma"},
	}

	// A goroutine that does some work concurrently.
	done := make(chan int, 1)
	go func() {
		acc := 0
		for i := 1; i <= 10; i++ {
			acc += i
		}
		done <- acc
	}()

	total := compute(points)
	goroutineSum := <-done

	fmt.Println("total:", total)
	fmt.Println("goroutineSum:", goroutineSum)
	fmt.Println("points:", points)
}
