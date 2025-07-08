# Gradient Frontend Integration Tests

This directory contains comprehensive NixOS integration tests for the Gradient frontend application.

## Test Coverage

The frontend integration test provides comprehensive coverage of all user interface components and functionality:

### Authentication & User Management
- Login page functionality and form validation
- Registration page with username availability checking
- Password strength validation and requirements
- User profile settings management
- Logout functionality

### Dashboard & Navigation
- Main dashboard navigation
- User dropdown menu functionality
- Organization card display and navigation
- Search functionality across pages

### Organization Management
- Organization settings page and form handling
- SSH public key display and copy functionality
- Organization member management (add, remove, role changes)
- Organization server management (add, edit, delete, enable/disable)
- Inline editing capabilities

### Project Management
- New project creation form
- Project settings and configuration
- Form validation and error handling

### Cache Management
- Cache listing and search functionality
- Cache creation and settings
- Cache metrics display

### Form Validations & Error Handling
- Client-side form validation
- Server-side error message display
- Empty form submission handling
- Real-time validation feedback

### JavaScript Functionality
- Dropdown menu interactions
- Search input functionality
- Dynamic form elements
- Real-time updates and polling

### Responsive Design
- Testing across different screen sizes (Desktop, Tablet, Mobile)
- Layout adaptation verification

## Test Implementation

The test uses:
- **NixOS Test Framework**: For service orchestration and system-level testing
- **Selenium WebDriver**: For browser automation and UI testing
- **Firefox**: As the primary browser for testing (headless mode)
- **PostgreSQL**: For database functionality
- **Nginx**: For reverse proxy and static file serving

## Test Environment

The test environment includes:
- Full Gradient server with API endpoints
- Gradient frontend Django application
- PostgreSQL database with proper authentication
- Nginx reverse proxy configuration
- X11 environment for browser testing (headless)

## Running the Tests

The tests are integrated into the main Gradient test suite and can be run with:

```bash
nix build .#checks.x86_64-linux.gradient-frontend
```

Or run interactively:

```bash
nix run .#checks.x86_64-linux.gradient-frontend.driver
```

## Test Structure

1. **Service Startup**: All required services are started and health-checked
2. **Test Data Setup**: Creates test users, organizations, and caches via API
3. **Browser Tests**: Comprehensive UI testing using Selenium
4. **Cleanup**: Proper cleanup of browser resources

## Expected Behavior

The tests verify that:
- All pages load without errors
- Forms submit correctly and show appropriate validation
- Navigation works across all sections
- Interactive elements (dropdowns, buttons) function properly
- Authentication flows work correctly
- Data is properly displayed and updated
- Error states are handled gracefully
- Responsive design works across screen sizes

## Troubleshooting

If tests fail:
1. Check service logs for startup issues
2. Verify database connectivity
3. Check API endpoints are responding
4. Verify browser driver compatibility
5. Check X11 display setup for headless testing

The test includes extensive logging and error handling to help diagnose issues.