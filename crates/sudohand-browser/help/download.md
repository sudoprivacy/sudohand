Use when you have a file URL; returns success, absolute path, filename and bytes after the file is saved, ready for the next step.

Fetches through the current page's browser session, subject to its cookies and CORS. Use download_link for a link or button that needs a trusted click. Path is a directory (default ./downloads). Waits up to 30 seconds; HTTP errors and incomplete downloads fail instead of reporting success. The returned path and byte count confirm completion; no browser verification call is needed.
