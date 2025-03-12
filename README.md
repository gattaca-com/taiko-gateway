# taiko-gateway

todo:
- forced inclusion
- error handling
- better docs


## How to run
Start taiko-geth with the simulator http rpc enabled:
```
./taiko-geth --http --http.addr "0.0.0.0" --http.port 8545 --http.api "eth,net,txpool,simulator"
```