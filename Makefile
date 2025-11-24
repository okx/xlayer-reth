-include .env
export

.PHONY: build-docker
build-docker:
	docker build -t xlayer-reth-node:latest -f DockerfileOp .

.PHONY: run-hello
run-hello:
	docker run --rm xlayer-reth-node:latest --help
