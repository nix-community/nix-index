#!/usr/bin/env python3
import sys
import json


CHUNK_SIZE = 4*32*1024


def wrong_written_size(x):
    out = 0
    while x >= 0:
        out += x
        x -= CHUNK_SIZE
    return out


if __name__ == '__main__':
    with open(sys.argv[1], 'rb') as f:
        data = f.read()

    print(sys.argv[1])
    try:
        json.loads(data)
    except json.JSONDecodeError as e:
        exc = e
        for margin in range(10):
            if len(data) == wrong_written_size(e.pos + margin):
                print(margin, exc, len(data), e.pos, data[e.pos:][:10], data[:10])
                sys.exit(0)

    sys.exit(1)
