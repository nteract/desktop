FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        dpkg-dev \
        gnupg \
        awscli \
        gzip \
        xz-utils \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY publish-apt.sh /usr/local/bin/publish-apt.sh
COPY prune-packages.py /usr/local/bin/prune-packages.py
RUN chmod +x /usr/local/bin/publish-apt.sh /usr/local/bin/prune-packages.py

ENTRYPOINT ["/usr/local/bin/publish-apt.sh"]
