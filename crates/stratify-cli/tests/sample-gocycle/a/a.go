package a

import "example.com/m/b"

func AThing() string {
	return b.BThing()
}
