from http.server import BaseHTTPRequestHandler, HTTPServer


class ReadyResponsesHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        self.send_response(200)
        self.end_headers()

    def do_POST(self) -> None:
        content = b'{"output":[{"type":"message"}]}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        self.wfile.write(content)

    def log_message(self, _format: str, *_args: object) -> None:
        pass


HTTPServer(("127.0.0.1", 38474), ReadyResponsesHandler).serve_forever()
