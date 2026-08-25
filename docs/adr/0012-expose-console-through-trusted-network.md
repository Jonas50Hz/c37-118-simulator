# Expose The Console Through The Trusted Network

The PMU Control Console defaults to `0.0.0.0:8081`, with IT guaranteeing that every reachable host interface is inside the Trusted Network Boundary. The console reverse-proxies same-origin `/api` requests to the Management Plane rather than enabling browser CORS on the simulator.