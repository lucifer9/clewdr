# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

ClewdR is a high-performance LLM proxy server built in Rust for Claude (Claude.ai, Claude Code) and Google Gemini APIs. It provides enterprise-level reliability with minimal resource usage, featuring a React-based web interface for management and multiple API compatibility layers.

## Development Commands

### Rust Backend

```bash
# Build the application
cargo build

# Build release version
cargo build --release

# Run in development mode
cargo run

# Check code without building
cargo check

# Update dependencies
cargo update

# Run with specific features
cargo run --features tokio-console  # Enable tokio console for debugging
cargo run --no-default-features --features embed-resource  # Embed frontend resources
```

### Frontend (React/TypeScript)

```bash
cd frontend

# Install dependencies
pnpm install

# Development server
pnpm dev

# Build for production
pnpm build

# Lint code
pnpm lint

# Preview production build
pnpm preview
```

### Testing & Quality

```bash
# Load testing (requires running server)
./load_test.sh

# Release process
./release.sh <version>  # e.g., ./release.sh 1.0.0
```

### Docker Development

```bash
# Build Docker image
docker build -t clewdr .

# Build with specific platform
docker buildx build --platform linux/amd64,linux/arm64 .
```

## Architecture Overview

### Core Components

**State Management Architecture:**
- `ClaudeWebState` - Manages Claude.ai web interface connections
- `ClaudeCodeState` - Handles Claude Code specific functionality  
- `GeminiState` - Manages Google Gemini API integrations
- Actor-based resource management with `CookieActorHandle` and `KeyActorHandle`

**Request Pipeline:**
1. **Router** (`src/router.rs`) - Routes requests to appropriate handlers
2. **Middleware Layer** - Authentication, rate limiting, request transformation
3. **Provider-Specific Processing** - Claude/Gemini specific logic
4. **Response Transformation** - Convert to OpenAI format or native format

**Key Directories:**
- `src/api/` - API endpoint handlers for different providers
- `src/middleware/` - Request/response middleware including Claude-to-OpenAI conversion
- `src/types/` - Type definitions for all supported APIs
- `src/services/` - Background services (cookie rotation, key management)
- `src/config/` - Configuration management
- `frontend/src/` - React web interface

### Provider Support

**Claude Integration:**
- Native Claude API format support
- OpenAI-compatible endpoints 
- Automatic cookie rotation and health monitoring
- System prompt caching and Extended Thinking mode
- Claude Code specialized endpoints at `/code/v1/`

**Gemini Integration:**
- AI Studio and Vertex AI support
- OAuth2 authentication for Vertex
- Native and OpenAI-compatible formats
- Automatic model detection and switching

### Configuration System

The application uses `figment` for configuration management with TOML files and environment variables. Configuration is centralized in `src/config/` with automatic reloading capabilities.

### Frontend Architecture

React application with:
- TypeScript for type safety
- Tailwind CSS for styling
- i18next for internationalization (English/Chinese)
- Vite for build tooling
- Real-time status monitoring and configuration management

## Important Implementation Details

**Resource Management:**
- Uses `moka` for intelligent caching
- Connection pooling with keep-alive optimization
- Event-driven architecture with Tokio async runtime

**Security Features:**
- Auto-generated admin passwords
- Bearer token authentication
- Chrome-level fingerprinting for API access
- Secure cookie and API key storage

**Performance Optimizations:**
- Static binary compilation with zero dependencies
- Memory usage <10MB in production
- Handles 1000+ requests/second
- Optimized build configuration in `Cargo.toml`

## Development Notes

**Feature Flags:**
- `tokio-console` - Enable tokio console debugging
- `embed-resource` - Embed frontend into binary 
- `external-resource` - Serve frontend from filesystem
- `mimalloc` - Use mimalloc allocator
- `self-update` - Enable self-update functionality

**Testing Strategy:**
The TODO.md outlines plans for comprehensive integration testing with mocked LLM endpoints using tools like `wiremock`.

**Architectural Improvements in Progress:**
- Refactoring middleware for better safety and clarity
- Unifying provider logic with trait-based system
- Automating frontend/backend type synchronization with `ts-rs`

## Build Features

The project supports multiple build configurations:
- Cross-platform compilation (Windows, macOS, Linux, Android)
- Docker multi-arch builds
- UPX compression for smaller binaries
- Optional memory profiling with `dhat`