# SPDX-License-Identifier: Apache-2.0
"""Upstream providers. Importing this package needs httpx (``pip install super-router[http]``)."""

from .anthropic import AnthropicProvider
from .openai import OpenAIProvider

__all__ = ["AnthropicProvider", "OpenAIProvider"]
