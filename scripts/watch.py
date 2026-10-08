#!/usr/bin/env python3
"""Run a brainfuck program which prints a P3 PPM image and watch it render.

Usage: scripts/watch.py [program.bf] [port]

Open the printed URL in a browser. Reloading the page replays the pixels
rendered so far.
"""

import http.server
import subprocess
import sys
import threading
import time

PROGRAM = sys.argv[1] if len(sys.argv) > 1 else "ray.bf"
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8000

PAGE = b"""<!doctype html>
<title>fucker</title>
<style>
  body { background: #222; color: #ccc; font: 14px monospace; margin: 2em; }
  canvas { image-rendering: pixelated; width: 100%; max-width: 1200px; display: block; }
</style>
<p id="status">waiting for header...</p>
<canvas id="canvas"></canvas>
<script>
  const canvas = document.getElementById("canvas");
  const status = document.getElementById("status");
  const context = canvas.getContext("2d");
  const started = Date.now();
  let image, total, filled = 0;

  const events = new EventSource("/events");
  events.addEventListener("size", (event) => {
    const [width, height] = event.data.split(",").map(Number);
    canvas.width = width;
    canvas.height = height;
    image = context.createImageData(width, height);
    total = width * height;
  });
  events.onmessage = (event) => {
    const values = event.data.split(",").map(Number);
    for (let i = 0; i + 2 < values.length && filled < total; i += 3, filled++) {
      image.data.set([values[i], values[i + 1], values[i + 2], 255], filled * 4);
    }
    context.putImageData(image, 0, 0);
    const seconds = (Date.now() - started) / 1000;
    status.textContent = `${filled} / ${total} pixels (${(100 * filled / total).toFixed(1)}%)`
      + `, ${(filled / seconds).toFixed(1)} pixels/s since page load`;
  };
  events.addEventListener("done", () => {
    status.textContent += " - done";
    events.close();
  });
</script>
"""

# State shared between the program reader and the HTTP handlers
lock = threading.Condition()
size = None
values = []  # flat r, g, b values
done = False


def read_program():
    global size, done
    process = subprocess.Popen(
        ["cargo", "run", "--release", "-q", "--", PROGRAM],
        stdout=subprocess.PIPE,
    )
    header = []
    for line in process.stdout:
        for token in line.split():
            if len(header) < 4:
                # "P3", width, height, maximum value
                header.append(token)
                if len(header) == 4:
                    with lock:
                        size = (int(header[1]), int(header[2]))
                        lock.notify_all()
            else:
                with lock:
                    values.append(int(token))
                    lock.notify_all()
    with lock:
        done = True
        lock.notify_all()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/":
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.end_headers()
            self.wfile.write(PAGE)
        elif self.path == "/events":
            self.stream()
        else:
            self.send_error(404)

    def stream(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()

        with lock:
            lock.wait_for(lambda: size is not None or done)
        if size:
            self.send(f"event: size\ndata: {size[0]},{size[1]}\n\n")

        sent = 0
        try:
            while True:
                with lock:
                    lock.wait_for(lambda: len(values) - sent >= 3 or done)
                    # Only whole pixels
                    end = len(values) - len(values) % 3
                    batch = values[sent:end]
                    finished = done and end == len(values)
                if batch:
                    self.send("data: " + ",".join(map(str, batch)) + "\n\n")
                    sent = end
                if finished:
                    self.send("event: done\ndata: \n\n")
                    return
                # Batch updates rather than sending every pixel.
                time.sleep(0.05)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def send(self, message):
        self.wfile.write(message.encode())
        self.wfile.flush()

    def log_message(self, *args):
        pass


threading.Thread(target=read_program, daemon=True).start()
server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
server.daemon_threads = True
print(f"Rendering {PROGRAM}: open http://127.0.0.1:{PORT}/")
server.serve_forever()
