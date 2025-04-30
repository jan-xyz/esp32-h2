build:
	cargo build --release

flash:
	cargo espflash flash --release

monitor:
	cargo espflash flash --monitor --release
