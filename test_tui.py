import os, select, socket, struct, subprocess, sys, time, fcntl, termios

def start_pty(cmd, rows=40, cols=120):
    a, b = pty_pair()
    set_winch(b, rows, cols)
    p = subprocess.Popen(cmd, stdin=b, stdout=b, stderr=b, preexec_fn=os.setsid, close_fds=True)
    os.close(b)
    return p, a

def pty_pair():
    m, s = socket.socketpair(socket.AF_UNIX, socket.SOCK_STREAM)
    # need real pty; use openpty instead
    pid_fd, fd2 = None, None
    return None

# simpler: use pty.openpty
import pty
def start(cmd, rows=40, cols=120):
    master, slave = pty.openpty()
    tname = os.ttyname(slave)
    winsize = struct.pack("HHHH", rows, cols, 0, 0)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, winsize)
    p = subprocess.Popen(cmd, stdin=slave, stdout=slave, stderr=slave, preexec_fn=os.setsid)
    os.close(slave)
    return p, master

def read_all(master, timeout=0.35):
    out = b""
    end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([master], [], [], 0.05)
        if r:
            try:
                d = os.read(master, 65536)
            except OSError:
                break
            if not d:
                break
            out += d
    return out.decode(errors="replace")

def strip_ansi(s):
    import re
    s = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", s)
    s = re.sub(r"\x1b][^\x07\x1b]*(\x07|\x1b\\)", "", s)
    s = re.sub(r"\x1b[()][A-Za-z0-9]", "", s)
    s = re.sub(r"[\x00-\x08\x0b-\x1f]", "", s)
    return s

BIN = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/repo-hub"
ROOTS = sys.argv[2] if len(sys.argv) > 2 else "/tmp/demo,/workspace"

p, m = start([BIN, "--roots", ROOTS])
time.sleep(2.5)
buf = strip_ansi(read_all(m, 1.0))
print("=== after startup ===")
print(buf[-3000:])
os.write(m, b"j")   # move down
time.sleep(0.6); read_all(m, 0.3)
os.write(m, b"j")
time.sleep(0.6); read_all(m, 0.3)
os.write(m, b"a")   # stage all on selected
time.sleep(1.5)
buf2 = strip_ansi(read_all(m, 1.0))
print("=== after 'a' (stage) ===")
print(buf2[-2500:])
os.write(m, b"\n")  # dismiss message popup
time.sleep(0.5); read_all(m, 0.3)
os.write(m, b"c")   # commit prompt
time.sleep(0.4); read_all(m, 0.3)
os.write(m, b"demo commit from repo-hub")
time.sleep(0.4); read_all(m, 0.3)
os.write(m, b"\r")  # Enter -> commit
time.sleep(1.5)
buf3 = strip_ansi(read_all(m, 1.0))
print("=== after commit ===")
print(buf3[-2500:])
os.write(m, b"\n")
time.sleep(0.4); read_all(m, 0.3)
os.write(m, b"/")   # search
time.sleep(0.3)
os.write(m, b"proj-b")
time.sleep(0.8)
buf4 = strip_ansi(read_all(m, 0.8))
print("=== after search proj-b ===")
print(buf4[-2000:])
os.write(m, b"\x1b")  # esc clears search
time.sleep(0.4); read_all(m, 0.3)
os.write(m, b"q")
time.sleep(0.8)
try:
    p.wait(timeout=3)
except Exception:
    p.kill()
print("exit code:", p.returncode)
