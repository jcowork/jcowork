.PHONY: build run test clean check clippy

build:
	cargo build --workspace

run: build
	cargo run --bin jcowork

test:
	cargo test --workspace

check:
	cargo check --workspace

clippy:
	cargo clippy --workspace -- -W clippy::all

clean:
	cargo clean

fmt:
	cargo fmt --all -- --check

fmt-fix:
	cargo fmt --all

# Database setup (creates data dir and runs migrations)
setup:
	mkdir -p ~/.jcowork/data

# Docker build
docker:
	docker build -t jcowork .

docker-up:
	docker compose up -d

docker-down:
	docker compose down
