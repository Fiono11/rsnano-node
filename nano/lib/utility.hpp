#pragma once

#include <nano/lib/locks.hpp>

#include <boost/current_function.hpp>
#include <boost/lexical_cast.hpp>
#include <boost/preprocessor/facilities/empty.hpp>
#include <boost/preprocessor/facilities/overload.hpp>

#include <filesystem>
#include <sstream>
#include <vector>

namespace boost
{
namespace system
{
	class error_code;
}

namespace program_options
{
	class options_description;
}
}

namespace rsnano
{
class ContainerInfoComponentHandle;
}

[[noreturn]] void assert_internal (char const * check_expr, char const * func, char const * file, unsigned int line, bool is_release_assert, std::string_view error = "");

#define release_assert_1(check) check ? (void)0 : assert_internal (#check, BOOST_CURRENT_FUNCTION, __FILE__, __LINE__, true)
#define release_assert_2(check, error_msg) check ? (void)0 : assert_internal (#check, BOOST_CURRENT_FUNCTION, __FILE__, __LINE__, true, error_msg)
#if !BOOST_PP_VARIADICS_MSVC
#define release_assert(...)                          \
	BOOST_PP_OVERLOAD (release_assert_, __VA_ARGS__) \
	(__VA_ARGS__)
#else
#define release_assert(...) BOOST_PP_CAT (BOOST_PP_OVERLOAD (release_assert_, __VA_ARGS__) (__VA_ARGS__), BOOST_PP_EMPTY ())
#endif

#ifdef NDEBUG
#define debug_assert(...) (void)0
#else
#define debug_assert_1(check) check ? (void)0 : assert_internal (#check, BOOST_CURRENT_FUNCTION, __FILE__, __LINE__, false)
#define debug_assert_2(check, error_msg) check ? (void)0 : assert_internal (#check, BOOST_CURRENT_FUNCTION, __FILE__, __LINE__, false, error_msg)
#if !BOOST_PP_VARIADICS_MSVC
#define debug_assert(...)                          \
	BOOST_PP_OVERLOAD (debug_assert_, __VA_ARGS__) \
	(__VA_ARGS__)
#else
#define debug_assert(...) BOOST_PP_CAT (BOOST_PP_OVERLOAD (debug_assert_, __VA_ARGS__) (__VA_ARGS__), BOOST_PP_EMPTY ())
#endif
#endif

namespace nano
{
/* These containers are used to collect information about sequence containers.
 * It makes use of the composite design pattern to collect information
 * from sequence containers and sequence containers inside member variables.
 */
struct container_info
{
	std::string name;
	size_t count;
	size_t sizeof_element;
};

class container_info_component
{
public:
	container_info_component (rsnano::ContainerInfoComponentHandle * handle_a);
	container_info_component (container_info_component && other_a);
	container_info_component (container_info_component const &) = delete;
	virtual ~container_info_component ();
	virtual bool is_composite () const = 0;
	rsnano::ContainerInfoComponentHandle * handle;
};

class container_info_composite : public container_info_component
{
public:
	container_info_composite (std::string const & name);
	container_info_composite (rsnano::ContainerInfoComponentHandle * handle_a);
	bool is_composite () const override;
	void add_component (std::unique_ptr<container_info_component> child);
	std::vector<std::unique_ptr<container_info_component>> get_children () const;
	std::string get_name () const;
};

class container_info_leaf : public container_info_component
{
public:
	container_info_leaf (container_info const & info);
	container_info_leaf (rsnano::ContainerInfoComponentHandle * handle_a);
	bool is_composite () const override;
	container_info const & get_info () const;

private:
	mutable container_info info;
	mutable bool info_loaded;
};

/*
 * Functions for managing filesystem permissions, platform specific
 */
void set_umask ();
void set_secure_perm_directory (std::filesystem::path const & path);
void set_secure_perm_directory (std::filesystem::path const & path, std::error_code & ec);
void set_secure_perm_file (std::filesystem::path const & path);
void set_secure_perm_file (std::filesystem::path const & path, std::error_code & ec);

/*
 * Function to check if running Windows as an administrator
 */
bool is_windows_elevated ();

/*
 * Function to check if the Windows Event log registry key exists
 */
bool event_log_reg_entry_exists ();

/*
 * Create the load memory addresses for the executable and shared libraries.
 */
void create_load_memory_address_files ();

/**
 * Some systems, especially in virtualized environments, may have very low file descriptor limits,
 * causing the node to fail. This function attempts to query the limit and returns the value. If the
 * limit cannot be queried, or running on a Windows system, this returns max-value of std::size_t.
 * Increasing the limit programmatically can be done only for the soft limit, the hard one requiring
 * super user permissions to modify.
 */
std::size_t get_file_descriptor_limit ();
void set_file_descriptor_limit (std::size_t limit);
/**
 * This should be called from entry points. It sets the file descriptor limit to the maximum allowed and logs any errors.
 */
constexpr std::size_t DEFAULT_FILE_DESCRIPTOR_LIMIT = 16384;
void initialize_file_descriptor_limit ();

template <class InputIt, class OutputIt, class Pred, class Func>
void transform_if (InputIt first, InputIt last, OutputIt dest, Pred pred, Func transform)
{
	while (first != last)
	{
		if (pred (*first))
		{
			*dest++ = transform (*first);
		}

		++first;
	}
}

/**
 * Erase elements from container when predicate returns true
 * TODO: Use `std::erase_if` in c++20
 */
template <class Container, class Pred>
void erase_if (Container & container, Pred pred)
{
	for (auto it = container.begin (), end = container.end (); it != end;)
	{
		if (pred (*it))
		{
			it = container.erase (it);
		}
		else
		{
			++it;
		}
	}
}

/** Safe narrowing cast which silences warnings and asserts on data loss in debug builds. This is optimized away. */
template <typename TARGET_TYPE, typename SOURCE_TYPE>
constexpr TARGET_TYPE narrow_cast (SOURCE_TYPE const & val)
{
	auto res (static_cast<TARGET_TYPE> (val));
	debug_assert (val == static_cast<SOURCE_TYPE> (res));
	return res;
}

// Issue #3748
void sort_options_description (const boost::program_options::options_description & source, boost::program_options::options_description & target);

using clock = std::chrono::steady_clock;

/**
 * Check whether time elapsed between `last` and `now` is greater than `duration`
 */
template <typename Duration>
bool elapsed (nano::clock::time_point const & last, Duration duration, nano::clock::time_point const & now)
{
	return last + duration < now;
}

/**
 * Check whether time elapsed since `last` is greater than `duration`
 */
template <typename Duration>
bool elapsed (nano::clock::time_point const & last, Duration duration)
{
	return elapsed (last, duration, nano::clock::now ());
}
}

namespace nano::util
{
/**
 * Joins elements with specified delimiter while transforming those elements via specified transform function
 */
template <class InputIt, class Func>
std::string join (InputIt first, InputIt last, std::string_view delimiter, Func transform)
{
	bool start = true;
	std::stringstream ss;
	while (first != last)
	{
		if (start)
		{
			start = false;
		}
		else
		{
			ss << delimiter;
		}
		ss << transform (*first);
		++first;
	}
	return ss.str ();
}

template <class Container, class Func>
std::string join (Container const & container, std::string_view delimiter, Func transform)
{
	return join (container.begin (), container.end (), delimiter, transform);
}

inline std::vector<std::string> split (std::string const & input, std::string_view delimiter)
{
	std::vector<std::string> result;
	std::size_t startPos = 0;
	std::size_t delimiterPos = input.find (delimiter, startPos);
	while (delimiterPos != std::string::npos)
	{
		std::string token = input.substr (startPos, delimiterPos - startPos);
		result.push_back (token);
		startPos = delimiterPos + delimiter.length ();
		delimiterPos = input.find (delimiter, startPos);
	}
	result.push_back (input.substr (startPos));
	return result;
}

template <class T>
std::string to_str (T const & val)
{
	return boost::lexical_cast<std::string> (val);
}
}
