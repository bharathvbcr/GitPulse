import http from "node:http";

const port = Number(process.argv[2]);
if (!Number.isInteger(port) || port <= 0) {
  console.error("usage: hold-port.mjs <port>");
  process.exit(2);
}

const server = http.createServer(() => {});
server.listen(port, "127.0.0.1", () => {
  process.stdout.write("ready\n");
});
