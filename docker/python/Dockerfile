# Sandbox for the Python profile (#5, profiles/python.toml): the gate runs
# each step in a fresh container of this image with --network none, as the
# host user, with the job's tree at /work. The repo's own dependencies live
# in /work/.venv (filled by #66), which comes first on PATH.
FROM python:3.12-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir \
      ruff==0.17.1 pytest==9.1.1 pytest-xdist==3.8.0 pytest-cov==7.1.0 \
      coverage==7.16.2 uv==0.13.4

RUN mkdir -p /home/aegis/.cache/uv && chmod -R a+rwX /home/aegis
ENV HOME=/home/aegis \
    PATH=/work/.venv/bin:/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin \
    PYTHONDONTWRITEBYTECODE=1 \
    UV_OFFLINE=1 \
    PIP_NO_INDEX=1
WORKDIR /work
