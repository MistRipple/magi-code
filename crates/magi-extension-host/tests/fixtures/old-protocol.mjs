import net from 'node:net';
const socket = net.createConnection(process.env.MAGI_EXT_ENDPOINT);
socket.on('connect', () => {
  const body = Buffer.from(JSON.stringify({
    jsonrpc: '2.0', id: 'h1', method: 'host.hello',
    params: { token: process.env.MAGI_EXT_TOKEN, protocol: { major: 2, minor: 0 }, extensionApi: { major: 1, minor: 0 }, features: [] },
  }));
  const header = Buffer.alloc(4);
  header.writeUInt32BE(body.length);
  socket.write(Buffer.concat([header, body]));
});
socket.on('data', () => process.exit(0));
socket.on('close', () => process.exit(0));
