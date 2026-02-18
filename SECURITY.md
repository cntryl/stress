# Security Policy

## Reporting Security Vulnerabilities

If you discover a security vulnerability in cntryl-stress, please **DO NOT** open a public GitHub issue. Instead, please report it responsibly by:

1. **Email**: Send details to the maintainers (look for security contact in the repository)
2. **GitHub Security Advisory**: Use GitHub's private vulnerability reporting feature:
   - Go to the repository
   - Click "Security" tab
   - Click "Report a vulnerability"
   - Fill out the form with details

## What to Include

Please provide:

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if you have one)
- Your contact information

## Response Timeline

We will:

1. **Acknowledge** your report within 48 hours
2. **Investigate** and confirm the vulnerability
3. **Develop** a fix for all affected versions
4. **Release** a patched version
5. **Credit** you in the security advisory (unless you prefer anonymity)

## Security Considerations

### When Using cntryl-stress

1. **Input Validation**: Ensure benchmark code doesn't execute untrusted code
2. **File Permissions**: Output files are written to `target/stress/` - ensure proper permissions
3. **Resource Limits**: Long-running benchmarks can consume significant memory/CPU
4. **Baseline Files**: Keep baseline JSON files secure if they contain sensitive benchmark data

### Dependencies

We keep dependencies minimal and up-to-date:

- Run `cargo update` regularly
- Use `cargo audit` to check for known vulnerabilities
- Report any dependency vulnerabilities found

## Supported Versions

| Version | Status | Security Updates |
|---------|--------|------------------|
| 0.1.x   | Current | Yes - all patches |
| < 0.1.0 | Legacy | No |

## Public Vulnerabilities

Once a fix is available, we will:

1. Release a patched version
2. Post a security advisory on GitHub
3. Credit the reporter (with permission)
4. Update this file if needed

## Security Best Practices for Contributors

- Don't commit secrets or credentials
- Use `git-secrets` or similar tools
- Review dependencies before adding them
- Follow Rust security guidelines
- Use clippy with `-- -D warnings`

## Questions?

- Check this policy first
- Look at existing security advisories
- Contact maintainers privately for security concerns

---

**Last Updated**: 2026-02-17
