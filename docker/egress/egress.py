"""Egress proxy of the fetch step (#66): the only way out of the fetch
container's network. It allows `CONNECT <host>:443` for the hosts in
AEGIS_ALLOW (comma-separated, exact names) and refuses everything else
with 403, including plain HTTP. Each decision is logged.

Runs with the standard library only, in the Python sandbox image:
    python -c "$(cat egress.py)"   (AEGIS_ALLOW=pypi.org,files.pythonhosted.org)
"""

import asyncio
import os
import sys

ALLOW = {h.strip().lower() for h in os.environ.get("AEGIS_ALLOW", "").split(",") if h.strip()}
PORT = int(os.environ.get("AEGIS_PORT", "3128"))
HEADER_LIMIT = 16 * 1024


def log(*parts):
    print(*parts, flush=True)


async def pipe(reader, writer):
    try:
        while data := await reader.read(65536):
            writer.write(data)
            await writer.drain()
    except (ConnectionError, asyncio.CancelledError):
        pass
    finally:
        writer.close()


async def refuse(writer, status, why):
    writer.write(f"HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".encode())
    await writer.drain()
    writer.close()
    log("deny", why)


async def handle(reader, writer):
    try:
        head = await asyncio.wait_for(reader.readuntil(b"\r\n\r\n"), timeout=30)
    except (asyncio.IncompleteReadError, asyncio.LimitOverrunError, asyncio.TimeoutError):
        writer.close()
        return
    if len(head) > HEADER_LIMIT:
        return await refuse(writer, "431 Request Header Fields Too Large", "oversized header")
    request = head.split(b"\r\n", 1)[0].decode("latin-1")
    parts = request.split()
    if len(parts) != 3 or parts[0] != "CONNECT":
        return await refuse(writer, "403 Forbidden", f"not CONNECT: {request[:200]}")
    host, _, port = parts[1].rpartition(":")
    host = host.strip("[]").lower()
    if port != "443" or host not in ALLOW:
        return await refuse(writer, "403 Forbidden", f"{host}:{port}")
    try:
        up_reader, up_writer = await asyncio.wait_for(asyncio.open_connection(host, 443), timeout=30)
    except (OSError, asyncio.TimeoutError) as e:
        return await refuse(writer, "502 Bad Gateway", f"{host}: {e}")
    log("allow", host)
    writer.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
    await writer.drain()
    await asyncio.gather(pipe(reader, up_writer), pipe(up_reader, writer))


async def main():
    server = await asyncio.start_server(handle, "0.0.0.0", PORT, limit=HEADER_LIMIT)
    log("listening", PORT, "allow:", ",".join(sorted(ALLOW)) or "(nothing)")
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        sys.exit(0)
