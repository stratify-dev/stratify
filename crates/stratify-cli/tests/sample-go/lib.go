package main

func neverCalled() string {
	return "dead"
}

func Exported() string {
	return "public api"
}
