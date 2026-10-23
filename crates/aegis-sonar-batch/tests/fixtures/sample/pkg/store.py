import os


def load(path):
    data = open(path).read()
    unused = 1
    return data


def save(path, data):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        f.write(data)
    os.replace(tmp, path)


class Store:
    def get(self, key):
        value = None
        return self._items[key]

    def put(self, key, value):
        self._items[key] = value
