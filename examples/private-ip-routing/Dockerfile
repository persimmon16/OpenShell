# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

FROM python:3.13-slim
RUN useradd -r -s /usr/sbin/nologin appuser
COPY server.py /app/server.py
EXPOSE 8080
USER appuser
CMD ["python", "/app/server.py"]
