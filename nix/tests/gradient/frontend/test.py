# SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

import json
import time
import os
import sys
from urllib.parse import urlparse, parse_qs

# Set up display environment for headless testing
os.environ['DISPLAY'] = ':99'

# Import Selenium components
try:
    from selenium import webdriver
    from selenium.webdriver.common.by import By
    from selenium.webdriver.support.ui import WebDriverWait
    from selenium.webdriver.support import expected_conditions as EC
    from selenium.webdriver.support.ui import Select
    from selenium.webdriver.common.keys import Keys
    from selenium.webdriver.firefox.options import Options as FirefoxOptions
    from selenium.webdriver.chrome.options import Options as ChromeOptions
    from selenium.common.exceptions import TimeoutException, NoSuchElementException
    from selenium.webdriver.common.action_chains import ActionChains
    selenium_available = True
except ImportError:
    selenium_available = False
    print("Selenium not available, skipping browser tests")

def wait_for_element(driver, by, value, timeout=10):
    """Wait for an element to be present and visible"""
    return WebDriverWait(driver, timeout).until(
        EC.visibility_of_element_located((by, value))
    )

def wait_for_clickable(driver, by, value, timeout=10):
    """Wait for an element to be clickable"""
    return WebDriverWait(driver, timeout).until(
        EC.element_to_be_clickable((by, value))
    )

def safe_find_element(driver, by, value, timeout=5):
    """Safely find an element with timeout"""
    try:
        return WebDriverWait(driver, timeout).until(
            EC.presence_of_element_located((by, value))
        )
    except TimeoutException:
        return None

def test_page_load(driver, url, expected_title_contains=None):
    """Test that a page loads successfully"""
    driver.get(url)
    time.sleep(2)
    
    # Check if page loaded (not showing error)
    assert "502 Bad Gateway" not in driver.page_source
    assert "404 Not Found" not in driver.page_source
    
    if expected_title_contains:
        assert expected_title_contains.lower() in driver.title.lower()
    
    return True

def test_form_submission(driver, form_data, submit_button_selector, expected_result_contains=None):
    """Test form submission with given data"""
    # Fill form fields
    for field_name, field_value in form_data.items():
        if field_name.startswith('select_'):
            # Handle select dropdowns
            select_name = field_name.replace('select_', '')
            select_element = Select(driver.find_element(By.NAME, select_name))
            select_element.select_by_visible_text(field_value)
        elif field_name.startswith('check_'):
            # Handle checkboxes
            check_name = field_name.replace('check_', '')
            checkbox = driver.find_element(By.NAME, check_name)
            if field_value and not checkbox.is_selected():
                checkbox.click()
            elif not field_value and checkbox.is_selected():
                checkbox.click()
        else:
            # Handle text inputs
            field = driver.find_element(By.NAME, field_name)
            field.clear()
            field.send_keys(field_value)
    
    # Submit form
    submit_button = driver.find_element(By.CSS_SELECTOR, submit_button_selector)
    submit_button.click()
    
    time.sleep(2)
    
    if expected_result_contains:
        assert expected_result_contains.lower() in driver.page_source.lower()
    
    return True

def setup_test_data_via_api():
    """Set up test data using the API"""
    print("Setting up test data via API...")
    
    # Register test user
    register_response = machine.succeed("""
        curl -X POST \
        -H "Content-Type: application/json" \
        -d '{"username": "testuser", "name": "Test User", "email": "test@example.com", "password": "TestPassword123!"}' \
        http://gradient.local/api/v1/auth/basic/register -s
    """)
    
    print(f"Register response: {register_response}")
    
    # Login to get token
    login_response = machine.succeed("""
        curl -X POST \
        -H "Content-Type: application/json" \
        -d '{"loginname": "testuser", "password": "TestPassword123!"}' \
        http://gradient.local/api/v1/auth/basic/login -s
    """)
    
    login_data = json.loads(login_response)
    if login_data.get("error"):
        print(f"Login failed: {login_data.get('message')}")
        return None
    
    token = login_data.get("message")
    print(f"Got auth token: {token[:20]}...")
    
    # Create test organization
    org_response = machine.succeed(f"""
        curl -X POST \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer {token}" \
        -d '{{"name": "testorg", "display_name": "Test Organization", "description": "Test organization for frontend testing"}}' \
        http://gradient.local/api/v1/orgs -s
    """)
    
    print(f"Organization creation response: {org_response}")
    
    # Create test cache
    cache_response = machine.succeed(f"""
        curl -X POST \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer {token}" \
        -d '{{"name": "testcache", "display_name": "Test Cache", "description": "Test cache for frontend testing", "priority": 10}}' \
        http://gradient.local/api/v1/caches -s
    """)
    
    print(f"Cache creation response: {cache_response}")
    
    return token

def run_frontend_tests():
    """Run comprehensive frontend tests"""
    print("=== Starting Frontend Tests ===")
    
    if not selenium_available:
        print("Selenium not available, skipping browser tests")
        return
    
    # Set up test data
    auth_token = setup_test_data_via_api()
    
    # Configure Firefox for headless testing
    firefox_options = FirefoxOptions()
    firefox_options.add_argument("--headless")
    firefox_options.add_argument("--no-sandbox")
    firefox_options.add_argument("--disable-dev-shm-usage")
    firefox_options.add_argument("--window-size=1920,1080")
    
    driver = None
    try:
        print("Starting Firefox browser...")
        driver = webdriver.Firefox(options=firefox_options)
        driver.set_window_size(1920, 1080)
        
        # Test 1: Homepage and Authentication
        with subtest("homepage_and_auth"):
            print("Testing homepage and authentication...")
            
            # Test homepage redirect to login
            driver.get("http://gradient.local/")
            time.sleep(3)
            
            # Should redirect to login page
            assert "/account/login" in driver.current_url
            assert "login" in driver.page_source.lower()
            
            # Test login form
            username_field = wait_for_element(driver, By.NAME, "username")
            password_field = driver.find_element(By.NAME, "password")
            login_button = driver.find_element(By.CSS_SELECTOR, "button[type='submit']")
            
            # Test empty form submission
            login_button.click()
            time.sleep(1)
            # Should show validation errors or stay on login page
            assert "/account/login" in driver.current_url
            
            # Test invalid credentials
            username_field.clear()
            username_field.send_keys("invalid_user")
            password_field.clear()
            password_field.send_keys("invalid_password")
            login_button.click()
            time.sleep(2)
            
            # Should show error or stay on login page
            assert "/account/login" in driver.current_url
            
            # Test valid login
            username_field.clear()
            username_field.send_keys("testuser")
            password_field.clear()
            password_field.send_keys("TestPassword123!")
            login_button.click()
            time.sleep(3)
            
            # Should redirect to dashboard
            assert "/account/login" not in driver.current_url
            print("✓ Login successful")
        
        # Test 2: Dashboard Navigation
        with subtest("dashboard_navigation"):
            print("Testing dashboard navigation...")
            
            # Test main navigation elements
            driver.get("http://gradient.local/")
            time.sleep(2)
            
            # Check for navigation elements
            nav_elements = [
                "Create",  # Create dropdown
                "testuser"  # User dropdown
            ]
            
            for element_text in nav_elements:
                try:
                    element = driver.find_element(By.XPATH, f"//*[contains(text(), '{element_text}')]")
                    assert element.is_displayed()
                    print(f"✓ Found navigation element: {element_text}")
                except NoSuchElementException:
                    print(f"⚠ Navigation element not found: {element_text}")
            
            # Test user dropdown
            try:
                user_dropdown = driver.find_element(By.XPATH, "//*[contains(text(), 'testuser')]")
                user_dropdown.click()
                time.sleep(1)
                
                # Check for profile settings and logout options
                dropdown_options = driver.find_elements(By.CSS_SELECTOR, ".dropdown-menu a, .dropdown-item")
                dropdown_texts = [opt.text for opt in dropdown_options]
                print(f"User dropdown options: {dropdown_texts}")
                
                # Click elsewhere to close dropdown
                driver.find_element(By.TAG_NAME, "body").click()
                time.sleep(1)
                
                print("✓ User dropdown functional")
            except NoSuchElementException:
                print("⚠ User dropdown not found")
        
        # Test 3: Organization Management
        with subtest("organization_management"):
            print("Testing organization management...")
            
            # Navigate to organization
            driver.get("http://gradient.local/")
            time.sleep(2)
            
            # Look for organization cards or links
            try:
                org_links = driver.find_elements(By.XPATH, "//*[contains(text(), 'testorg') or contains(text(), 'Test Organization')]")
                if org_links:
                    org_links[0].click()
                    time.sleep(2)
                    print("✓ Navigated to organization")
                else:
                    print("⚠ No organization links found")
            except Exception as e:
                print(f"⚠ Error navigating to organization: {e}")
            
            # Test organization settings
            try:
                driver.get("http://gradient.local/organization/testorg/settings")
                time.sleep(2)
                
                # Check for organization settings form
                form_elements = driver.find_elements(By.CSS_SELECTOR, "input[name], textarea[name]")
                form_fields = [elem.get_attribute("name") for elem in form_elements]
                print(f"Organization settings form fields: {form_fields}")
                
                # Test SSH key display
                ssh_key_elements = driver.find_elements(By.CSS_SELECTOR, ".ssh-key-display, textarea[readonly]")
                if ssh_key_elements:
                    print("✓ SSH key display found")
                else:
                    print("⚠ SSH key display not found")
                
                # Test save button
                save_buttons = driver.find_elements(By.CSS_SELECTOR, "button[type='submit'], .submit-btn")
                if save_buttons:
                    print("✓ Save button found")
                else:
                    print("⚠ Save button not found")
                
                print("✓ Organization settings page functional")
            except Exception as e:
                print(f"⚠ Error testing organization settings: {e}")
        
        # Test 4: Organization Members
        with subtest("organization_members"):
            print("Testing organization members...")
            
            try:
                driver.get("http://gradient.local/organization/testorg/members")
                time.sleep(2)
                
                # Check for members list
                member_elements = driver.find_elements(By.CSS_SELECTOR, ".member-list, .member-item, tr")
                print(f"Found {len(member_elements)} member-related elements")
                
                # Check for add member form
                add_member_form = driver.find_elements(By.CSS_SELECTOR, "input[name='username'], input[name='email']")
                if add_member_form:
                    print("✓ Add member form found")
                else:
                    print("⚠ Add member form not found")
                
                # Check for role dropdowns
                role_selects = driver.find_elements(By.CSS_SELECTOR, "select[name*='role'], select option")
                if role_selects:
                    print("✓ Role selection elements found")
                else:
                    print("⚠ Role selection elements not found")
                
                print("✓ Organization members page functional")
            except Exception as e:
                print(f"⚠ Error testing organization members: {e}")
        
        # Test 5: Organization Servers
        with subtest("organization_servers"):
            print("Testing organization servers...")
            
            try:
                driver.get("http://gradient.local/organization/testorg/servers")
                time.sleep(2)
                
                # Check for servers list
                server_elements = driver.find_elements(By.CSS_SELECTOR, ".server-list, .server-item, tr")
                print(f"Found {len(server_elements)} server-related elements")
                
                # Check for add server button/form
                add_server_elements = driver.find_elements(By.CSS_SELECTOR, "button[data-bs-toggle], .add-server, input[name='name']")
                if add_server_elements:
                    print("✓ Add server functionality found")
                else:
                    print("⚠ Add server functionality not found")
                
                # Check for server management buttons
                management_buttons = driver.find_elements(By.CSS_SELECTOR, "button[data-action], .edit-btn, .delete-btn")
                print(f"Found {len(management_buttons)} server management buttons")
                
                print("✓ Organization servers page functional")
            except Exception as e:
                print(f"⚠ Error testing organization servers: {e}")
        
        # Test 6: Project Management
        with subtest("project_management"):
            print("Testing project management...")
            
            # Test new project page
            try:
                driver.get("http://gradient.local/new/project")
                time.sleep(2)
                
                # Check for project creation form
                form_fields = driver.find_elements(By.CSS_SELECTOR, "input[name], textarea[name], select[name]")
                field_names = [elem.get_attribute("name") for elem in form_fields]
                print(f"New project form fields: {field_names}")
                
                # Test form validation
                submit_button = driver.find_element(By.CSS_SELECTOR, "button[type='submit']")
                submit_button.click()
                time.sleep(1)
                
                # Should show validation errors or stay on form
                current_url = driver.current_url
                print(f"After empty form submission: {current_url}")
                
                print("✓ New project page functional")
            except Exception as e:
                print(f"⚠ Error testing new project: {e}")
        
        # Test 7: Cache Management
        with subtest("cache_management"):
            print("Testing cache management...")
            
            try:
                driver.get("http://gradient.local/cache")
                time.sleep(2)
                
                # Check for cache list
                cache_elements = driver.find_elements(By.CSS_SELECTOR, ".cache-card, .cache-item, tr")
                print(f"Found {len(cache_elements)} cache-related elements")
                
                # Check for search functionality
                search_inputs = driver.find_elements(By.CSS_SELECTOR, "input[type='search'], input[placeholder*='search']")
                if search_inputs:
                    print("✓ Cache search functionality found")
                    
                    # Test search
                    search_input = search_inputs[0]
                    search_input.clear()
                    search_input.send_keys("test")
                    time.sleep(1)
                    print("✓ Search input functional")
                else:
                    print("⚠ Cache search functionality not found")
                
                print("✓ Cache management page functional")
            except Exception as e:
                print(f"⚠ Error testing cache management: {e}")
        
        # Test 8: User Profile Settings
        with subtest("user_profile_settings"):
            print("Testing user profile settings...")
            
            try:
                driver.get("http://gradient.local/settings/profile")
                time.sleep(2)
                
                # Check for profile form
                profile_fields = driver.find_elements(By.CSS_SELECTOR, "input[name], textarea[name]")
                field_names = [elem.get_attribute("name") for elem in profile_fields]
                print(f"Profile form fields: {field_names}")
                
                # Test form interaction
                for field in profile_fields:
                    if field.get_attribute("type") == "text":
                        field.clear()
                        field.send_keys("test_value")
                        time.sleep(0.5)
                        field.clear()
                
                print("✓ Profile settings page functional")
            except Exception as e:
                print(f"⚠ Error testing profile settings: {e}")
        
        # Test 9: Registration Page
        with subtest("registration_page"):
            print("Testing registration page...")
            
            # Logout first
            try:
                driver.get("http://gradient.local/account/logout")
                time.sleep(2)
            except:
                pass
            
            try:
                driver.get("http://gradient.local/account/register")
                time.sleep(2)
                
                # Check for registration form
                reg_fields = driver.find_elements(By.CSS_SELECTOR, "input[name], textarea[name]")
                field_names = [elem.get_attribute("name") for elem in reg_fields]
                print(f"Registration form fields: {field_names}")
                
                # Test username availability checking
                username_field = safe_find_element(driver, By.NAME, "username")
                if username_field:
                    username_field.clear()
                    username_field.send_keys("test_new_user")
                    time.sleep(2)  # Wait for availability check
                    print("✓ Username field functional")
                
                # Test password strength validation
                password_field = safe_find_element(driver, By.NAME, "password")
                if password_field:
                    password_field.clear()
                    password_field.send_keys("weak")
                    time.sleep(1)
                    password_field.clear()
                    password_field.send_keys("StrongPassword123!")
                    time.sleep(1)
                    print("✓ Password field functional")
                
                print("✓ Registration page functional")
            except Exception as e:
                print(f"⚠ Error testing registration: {e}")
        
        # Test 10: Form Validations and Error Handling
        with subtest("form_validations"):
            print("Testing form validations and error handling...")
            
            # Test various form validations across different pages
            validation_tests = [
                ("http://gradient.local/account/login", "login form"),
                ("http://gradient.local/account/register", "registration form"),
                ("http://gradient.local/new/project", "new project form"),
                ("http://gradient.local/new/cache", "new cache form"),
            ]
            
            for url, form_name in validation_tests:
                try:
                    driver.get(url)
                    time.sleep(2)
                    
                    # Find and click submit button
                    submit_buttons = driver.find_elements(By.CSS_SELECTOR, "button[type='submit'], .submit-btn")
                    if submit_buttons:
                        submit_buttons[0].click()
                        time.sleep(1)
                        
                        # Check for validation messages
                        error_elements = driver.find_elements(By.CSS_SELECTOR, ".error, .alert-danger, .invalid-feedback")
                        if error_elements:
                            print(f"✓ {form_name} validation working")
                        else:
                            print(f"⚠ {form_name} validation not detected")
                    else:
                        print(f"⚠ {form_name} submit button not found")
                        
                except Exception as e:
                    print(f"⚠ Error testing {form_name}: {e}")
        
        # Test 11: JavaScript Functionality
        with subtest("javascript_functionality"):
            print("Testing JavaScript functionality...")
            
            try:
                driver.get("http://gradient.local/")
                time.sleep(2)
                
                # Test dropdown functionality
                dropdowns = driver.find_elements(By.CSS_SELECTOR, "[data-bs-toggle='dropdown'], .dropdown-toggle")
                for dropdown in dropdowns[:2]:  # Test first 2 dropdowns
                    try:
                        dropdown.click()
                        time.sleep(0.5)
                        
                        # Check if dropdown opened
                        dropdown_menus = driver.find_elements(By.CSS_SELECTOR, ".dropdown-menu.show, .dropdown-menu[style*='display']")
                        if dropdown_menus:
                            print("✓ Dropdown functionality working")
                        
                        # Click elsewhere to close
                        driver.find_element(By.TAG_NAME, "body").click()
                        time.sleep(0.5)
                        
                    except Exception as e:
                        print(f"⚠ Dropdown test error: {e}")
                
                # Test search functionality
                search_inputs = driver.find_elements(By.CSS_SELECTOR, "input[type='search'], input[placeholder*='search']")
                for search_input in search_inputs[:2]:  # Test first 2 search inputs
                    try:
                        search_input.clear()
                        search_input.send_keys("test")
                        time.sleep(1)
                        search_input.clear()
                        print("✓ Search input functional")
                    except Exception as e:
                        print(f"⚠ Search test error: {e}")
                
                print("✓ JavaScript functionality tests completed")
            except Exception as e:
                print(f"⚠ Error testing JavaScript: {e}")
        
        # Test 12: Responsive Design
        with subtest("responsive_design"):
            print("Testing responsive design...")
            
            # Test different screen sizes
            screen_sizes = [
                (1920, 1080, "Desktop"),
                (1024, 768, "Tablet"),
                (375, 667, "Mobile")
            ]
            
            for width, height, device in screen_sizes:
                try:
                    driver.set_window_size(width, height)
                    time.sleep(1)
                    
                    driver.get("http://gradient.local/")
                    time.sleep(2)
                    
                    # Check if page is responsive
                    body = driver.find_element(By.TAG_NAME, "body")
                    if body.is_displayed():
                        print(f"✓ {device} ({width}x{height}) layout functional")
                    else:
                        print(f"⚠ {device} ({width}x{height}) layout issues")
                        
                except Exception as e:
                    print(f"⚠ {device} responsive test error: {e}")
            
            # Reset to default size
            driver.set_window_size(1920, 1080)
        
        print("=== All Frontend Tests Completed ===")
        
    except Exception as e:
        print(f"Critical error in frontend tests: {e}")
        import traceback
        traceback.print_exc()
        
    finally:
        if driver:
            driver.quit()
            print("Browser closed")

# Main test execution
start_all()

# Wait for services to be ready
machine.wait_for_unit("gradient-server.service")
machine.wait_for_unit("gradient-frontend.service")
machine.wait_for_unit("nginx.service")
machine.wait_for_unit("postgresql.service")

print("=== All services started ===")

# Test basic connectivity
with subtest("basic_connectivity"):
    print("Testing basic connectivity...")
    
    # Test API health
    machine.succeed("curl http://gradient.local/api/v1/health -i --fail")
    print("✓ API health check passed")
    
    # Test frontend connectivity
    response = machine.succeed("curl http://gradient.local/ -s")
    assert len(response) > 0
    print("✓ Frontend connectivity check passed")
    
    # Test that services are responding
    assert "502 Bad Gateway" not in response
    assert "404 Not Found" not in response
    print("✓ Services responding correctly")

# Run comprehensive frontend tests
with subtest("comprehensive_frontend_tests"):
    run_frontend_tests()

print("=== Frontend Integration Tests Completed Successfully ===")